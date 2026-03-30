import { useState, useEffect, useRef } from 'react'
import { useTranslation } from 'react-i18next'
import { invoke } from '@tauri-apps/api/core'
import { ChevronDown, Edit2, X, UploadCloud, Check } from 'lucide-react'
import type { CharacterMeta, AgentInfo } from '../lib/types'
import { getStore, loadCharacters, saveCharacters, getActiveCharacter, setActiveCharacter, fileToDataUrl, MINI_CATEGORIES, CUSTOM_ASSET_PREFIX, DEFAULT_CHAR_NAME } from '../lib/store'

export function CharacterTab({ activeTab }: { activeTab: 'pet' | 'mini' }) {
  const { t } = useTranslation()
  const [characters, setCharacters] = useState<CharacterMeta[]>([])
  const [active, setActive] = useState('')

  const [agentList, setAgentList] = useState<AgentInfo[]>([])
  const [trackedAgent, setTrackedAgent] = useState('main')

  // Pet upload state
  const [newName, setNewName] = useState('')
  const [workFiles, setWorkFiles] = useState<File[]>([])
  const [restFiles, setRestFiles] = useState<File[]>([])
  const [workPreviews, setWorkPreviews] = useState<string[]>([])
  const [restPreviews, setRestPreviews] = useState<string[]>([])
  const [saving, setSaving] = useState(false)
  const workInputRef = useRef<HTMLInputElement>(null)
  const restInputRef = useRef<HTMLInputElement>(null)

  // Mini upload state
  const [miniCharName, setMiniCharName] = useState('')
  const [miniCategory, setMiniCategory] = useState('walk')
  const [miniFiles, setMiniFiles] = useState<File[]>([])
  const [miniPreviews, setMiniPreviews] = useState<string[]>([])
  const [miniSaving, setMiniSaving] = useState(false)
  const miniInputRef = useRef<HTMLInputElement>(null)

  const reload = async () => {
    const chars = await loadCharacters()
    setCharacters(chars)
    const act = await getActiveCharacter()
    setActive(act)
    const store = await getStore()
    const ta = ((await store.get('tracked_agent')) as string) || 'main'
    setTrackedAgent(ta)
    try {
      const agents = (await invoke('get_agents')) as AgentInfo[]
      setAgentList(agents)
    } catch { /* agents not available */ }
  }

  useEffect(() => { reload() }, [])

  const handleSelect = async (name: string) => {
    await setActiveCharacter(name)
    setActive(name)
  }

  const handleTrackedAgentChange = async (agentId: string) => {
    setTrackedAgent(agentId)
    const store = await getStore()
    await store.set('tracked_agent', agentId)
    await store.save()
    localStorage.setItem('ooclaw_tracked_agent', agentId)
  }

  const handleDelete = async (name: string) => {
    const char = characters.find((c) => c.name === name)
    if (!char || char.builtin) return
    const next = characters.filter((c) => c.name !== name)
    await saveCharacters(next)
    if (active === name) {
      await setActiveCharacter(DEFAULT_CHAR_NAME)
      setActive(DEFAULT_CHAR_NAME)
    }
    setCharacters(next)
    try { await invoke('delete_character_assets', { name }) } catch { /* ignore */ }
  }

  const handleDeleteMiniGif = async (charName: string, cat: string, gifPath: string) => {
    if (cat === 'walk') return
    const char = characters.find((c) => c.name === charName)
    if (!char || !char.miniActions) return
    const newGifs = char.miniActions[cat].filter((g) => g !== gifPath)
    const newActions = { ...char.miniActions, [cat]: newGifs }
    if (newGifs.length === 0) delete newActions[cat]
    const updated = characters.map((c) =>
      c.name === charName ? { ...c, miniActions: Object.keys(newActions).length > 0 ? newActions : undefined } : c
    )
    await saveCharacters(updated)
    setCharacters(updated)
    const fileName = gifPath.split('/').pop() || ''
    try { await invoke('delete_character_gif', { charName, subfolder: `mini/${cat}`, fileName }) } catch { /* ignore */ }
  }

  const handleDeletePetGif = async (charName: string, category: 'rest' | 'crawl', gifPath: string) => {
    const char = characters.find((c) => c.name === charName)
    if (!char) return
    const field = category === 'rest' ? 'restGifs' : 'crawlGifs'
    const current = (category === 'rest' ? char.restGifs : char.crawlGifs) || []
    const newGifs = current.filter((g) => g !== gifPath)
    const updated = characters.map((c) =>
      c.name === charName ? { ...c, [field]: newGifs } : c
    )
    await saveCharacters(updated)
    setCharacters(updated)
    const fileName = gifPath.split('/').pop() || ''
    const subfolder = category === 'rest' ? 'pet/rest' : 'pet/crawl'
    try { await invoke('delete_character_gif', { charName, subfolder, fileName }) } catch { /* ignore */ }
  }

  // Pet upload handlers
  const handleWorkFiles = async (files: FileList | null) => {
    if (!files) return
    const arr = Array.from(files).filter((f) => f.name.endsWith('.gif'))
    setWorkFiles(arr)
    setWorkPreviews(await Promise.all(arr.map(fileToDataUrl)))
  }

  const handleRestFiles = async (files: FileList | null) => {
    if (!files) return
    const arr = Array.from(files).filter((f) => f.name.endsWith('.gif'))
    setRestFiles(arr)
    setRestPreviews(await Promise.all(arr.map(fileToDataUrl)))
  }

  const handlePetUpload = async () => {
    const name = newName.trim()
    if (!name || workFiles.length === 0 || restFiles.length === 0) return
    if (characters.some((c) => c.name === name)) {
      alert(t('character.charNameExists'))
    }
    setSaving(true)
    try {
      const existing = characters.find((c) => c.name === name)
      const workPaths: string[] = existing ? [...existing.workGifs] : []
      const restPaths: string[] = existing ? [...existing.restGifs] : []
      for (const f of workFiles) {
        const data = await fileToDataUrl(f)
        await invoke('save_character_gif', { charName: name, fileName: f.name, subfolder: 'pet/work', dataUrl: data })
        workPaths.push(`${CUSTOM_ASSET_PREFIX}/${name}/pet/work/${f.name}`)
      }
      for (const f of restFiles) {
        const data = await fileToDataUrl(f)
        await invoke('save_character_gif', { charName: name, fileName: f.name, subfolder: 'pet/rest', dataUrl: data })
        restPaths.push(`${CUSTOM_ASSET_PREFIX}/${name}/pet/rest/${f.name}`)
      }
      let updated: CharacterMeta[]
      if (existing) {
        updated = characters.map((c) => (c.name === name ? { ...c, workGifs: workPaths, restGifs: restPaths } : c))
      } else {
        updated = [...characters, { name, workGifs: workPaths, restGifs: restPaths }]
      }
      await saveCharacters(updated)
      setCharacters(updated)
      setNewName(''); setWorkFiles([]); setRestFiles([]); setWorkPreviews([]); setRestPreviews([])
    } catch (e: any) { alert(t('character.saveFailed') + ' ' + String(e)) }
    setSaving(false)
  }

  // Mini upload handlers
  const handleMiniFiles = async (files: FileList | null) => {
    if (!files) return
    const arr = Array.from(files).filter((f) => f.name.endsWith('.gif'))
    setMiniFiles(arr)
    setMiniPreviews(await Promise.all(arr.map(fileToDataUrl)))
  }

  const handleMiniUpload = async () => {
    const name = miniCharName.trim()
    const cat = miniCategory.trim()
    if (!name || !cat || miniFiles.length === 0) return
    setMiniSaving(true)
    try {
      const gifPaths: string[] = []
      for (const f of miniFiles) {
        const data = await fileToDataUrl(f)
        await invoke('save_character_gif', { charName: name, fileName: f.name, subfolder: `mini/${cat}`, dataUrl: data })
        gifPaths.push(`${CUSTOM_ASSET_PREFIX}/${name}/mini/${cat}/${f.name}`)
      }
      const existing = characters.find((c) => c.name === name)
      let updated: CharacterMeta[]
      if (existing) {
        const newActions = { ...(existing.miniActions || {}), [cat]: gifPaths }
        updated = characters.map((c) => (c.name === name ? { ...c, miniActions: newActions } : c))
      } else {
        updated = [...characters, { name, workGifs: [], restGifs: [], miniActions: { [cat]: gifPaths } }]
      }
      await saveCharacters(updated)
      setCharacters(updated)
      setMiniFiles([]); setMiniPreviews([])
    } catch (e: any) { alert(t('character.saveFailed') + ' ' + String(e)) }
    setMiniSaving(false)
  }

  const charsWithPet = characters.filter((c) => c.workGifs.length > 0 || c.restGifs.length > 0)
  const charsWithMini = characters.filter((c) => c.miniActions && Object.keys(c.miniActions).length > 0)

  return (
    <div className="flex-1 overflow-auto p-8">
      {activeTab === 'pet' ? (
        <div className="max-w-4xl mx-auto space-y-8 pb-12">
          {/* Track Agent */}
          <section className="bg-white rounded-xl border border-gray-200 p-5 shadow-sm">
            <div className="flex items-center justify-between">
              <div>
                <h2 className="text-sm font-semibold text-gray-900">{t('character.trackAgent')}</h2>
                <p className="text-xs text-gray-500 mt-1">{t('character.trackAgentDesc')}</p>
              </div>
              <AgentSelect
                agents={agentList}
                value={trackedAgent}
                onChange={handleTrackedAgentChange}
              />
            </div>
          </section>

          {/* Character Cards */}
          <section>
            <h2 className="text-base font-semibold text-gray-900 mb-4">{t('character.currentPetChar')}</h2>
            <div className="space-y-6">
              {charsWithPet.map((c) => (
                <PetCharacterCard
                  key={c.name}
                  character={c}
                  isActive={active === c.name}
                  onSelect={() => handleSelect(c.name)}
                  onDelete={() => handleDelete(c.name)}
                  onDeleteGif={handleDeletePetGif}
                />
              ))}
            </div>
          </section>

          {/* Upload Pet GIF */}
          <section>
            <h2 className="text-base font-semibold text-gray-900 mb-4">{t('character.uploadPetGif')}</h2>
            <div className="bg-white rounded-xl border border-gray-200 shadow-sm p-6">
              <div className="space-y-6">
                <div>
                  <label className="block text-sm font-medium text-gray-700 mb-2">{t('character.charName')}</label>
                  <input
                    type="text"
                    value={newName}
                    onChange={(e) => setNewName(e.target.value)}
                    placeholder={t('character.charNamePlaceholder')}
                    className="w-full bg-gray-50 border border-gray-200 text-gray-900 text-sm rounded-lg focus:ring-2 focus:ring-gray-900/10 focus:border-gray-900 block p-2.5 outline-none transition-all placeholder:text-gray-400"
                  />
                </div>

                <div className="grid grid-cols-2 gap-6">
                  <UploadZone label={t('character.workGif')} inputRef={workInputRef} previews={workPreviews} onFiles={handleWorkFiles} />
                  <UploadZone label={t('character.restGif')} inputRef={restInputRef} previews={restPreviews} onFiles={handleRestFiles} />
                </div>

                <div className="pt-2">
                  <button
                    onClick={handlePetUpload}
                    disabled={saving || !newName.trim() || workFiles.length === 0 || restFiles.length === 0}
                    className="bg-gray-900 hover:bg-gray-800 text-white text-sm font-medium py-2.5 px-6 rounded-lg transition-colors shadow-sm disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    {saving ? t('common.saving') : t('character.uploadFiles')}
                  </button>
                </div>
              </div>
            </div>
          </section>
        </div>
      ) : (
        <MiniTabContent
          characters={charsWithMini}
          onDelete={handleDelete}
          onDeleteMiniGif={handleDeleteMiniGif}
          miniCharName={miniCharName}
          setMiniCharName={setMiniCharName}
          miniCategory={miniCategory}
          setMiniCategory={setMiniCategory}
          miniInputRef={miniInputRef}
          miniPreviews={miniPreviews}
          onMiniFiles={handleMiniFiles}
          onMiniUpload={handleMiniUpload}
          miniSaving={miniSaving}
        />
      )}
    </div>
  )
}

// ─── Pet Character Card ───

function PetCharacterCard({
  character, isActive, onSelect, onDelete, onDeleteGif
}: {
  character: CharacterMeta
  isActive: boolean
  onSelect: () => void
  onDelete: () => void
  onDeleteGif: (charName: string, category: 'rest' | 'crawl', gifPath: string) => void
}) {
  const { t } = useTranslation()
  const [isEditing, setIsEditing] = useState(false)

  return (
    <div
      className={`bg-white rounded-xl border border-gray-200 shadow-sm overflow-hidden transition-colors ${isActive ? 'ring-2 ring-emerald-500/30' : ''}`}
      onClick={onSelect}
    >
      <div className="px-6 py-4 border-b border-gray-100 flex items-center justify-between bg-gray-50/50">
        <div className="flex items-center space-x-3">
          <h3 className="text-sm font-semibold text-gray-900">{character.name}</h3>
          {isActive && (
            <span className="inline-flex items-center px-2 py-0.5 rounded text-[11px] font-medium bg-emerald-50 text-emerald-600 border border-emerald-200/60">
              {t('character.inUse')}
            </span>
          )}
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={(e) => { e.stopPropagation(); setIsEditing(!isEditing) }}
            className={`flex items-center gap-1.5 text-xs font-medium px-3 py-1.5 rounded-md transition-colors ${
              isEditing
                ? 'bg-gray-900 text-white hover:bg-gray-800'
                : 'bg-white border border-gray-200 text-gray-600 hover:bg-gray-50'
            }`}
          >
            {isEditing ? <>{t('common.done')}</> : <><Edit2 size={12} /> {t('character.editImages')}</>}
          </button>
          {!character.builtin && (
            <button
              onClick={(e) => { e.stopPropagation(); onDelete() }}
              className="text-gray-400 hover:text-red-500 transition-colors p-1"
              title={t('character.deleteChar')}
            >
              <X size={16} />
            </button>
          )}
        </div>
      </div>

      <div className="p-6 space-y-8">
        <StateGroup title="work" gifs={character.workGifs} isEditing={false} onDelete={() => {}} />
        <StateGroup title="rest" gifs={character.restGifs} isEditing={isEditing} onDelete={(g) => onDeleteGif(character.name, 'rest', g)} />
        {(character.crawlGifs?.length ?? 0) > 0 && (
          <StateGroup title="crawl" gifs={character.crawlGifs!} isEditing={isEditing} onDelete={(g) => onDeleteGif(character.name, 'crawl', g)} />
        )}
        {character.workGifs.length === 0 && character.restGifs.length === 0 && (
          <div className="text-sm text-gray-400 text-center py-4">{t('character.noImages')}</div>
        )}
      </div>
    </div>
  )
}

// ─── State Group (for Pet) ───

function StateGroup({ title, gifs, isEditing, onDelete }: {
  title: string; gifs: string[]; isEditing: boolean; onDelete: (gifPath: string) => void
}) {
  const { t } = useTranslation()
  if (gifs.length === 0) return null
  return (
    <div>
      <div className="flex items-center gap-2 mb-3">
        <h4 className="text-sm font-medium text-gray-700 capitalize">{title}</h4>
        <span className="text-[11px] font-medium bg-gray-100 text-gray-500 px-1.5 py-0.5 rounded-md">{gifs.length}</span>
      </div>
      <div className="flex flex-wrap gap-4">
        {gifs.map((g, i) => (
          <div key={i} className="relative group">
            <div className={`w-16 h-16 rounded-xl border flex items-center justify-center overflow-hidden transition-colors ${
              isEditing ? 'border-red-200 bg-red-50/30' : 'border-gray-200 bg-gray-50 group-hover:border-gray-300'
            }`}>
              {g && <img src={g} alt="" className="w-12 h-12 object-contain" draggable={false} />}
            </div>
            {isEditing && (
              <button
                onClick={(e) => { e.stopPropagation(); onDelete(g) }}
                className="absolute -top-2 -right-2 bg-red-500 text-white border-2 border-white hover:bg-red-600 rounded-full p-0.5 shadow-sm z-10 transition-transform hover:scale-110"
                title={t('common.delete')}
              >
                <X size={12} strokeWidth={3} />
              </button>
            )}
          </div>
        ))}
      </div>
    </div>
  )
}

// ─── Mini Tab Content ───

function MiniTabContent({
  characters, onDelete, onDeleteMiniGif,
  miniCharName, setMiniCharName, miniCategory, setMiniCategory,
  miniInputRef, miniPreviews, onMiniFiles, onMiniUpload, miniSaving,
}: {
  characters: CharacterMeta[]
  onDelete: (name: string) => void
  onDeleteMiniGif: (charName: string, cat: string, gifPath: string) => void
  miniCharName: string; setMiniCharName: (v: string) => void
  miniCategory: string; setMiniCategory: (v: string) => void
  miniInputRef: React.RefObject<HTMLInputElement | null>
  miniPreviews: string[]
  onMiniFiles: (files: FileList | null) => void
  onMiniUpload: () => void
  miniSaving: boolean
}) {
  const { t } = useTranslation()
  const [isEditing, setIsEditing] = useState(false)
  const [miniModeChar, setMiniModeChar] = useState('')

  useEffect(() => {
    ;(async () => {
      const store = await getStore()
      const mc = ((await store.get('mini_character')) as string) || ''
      setMiniModeChar(mc)
    })()
  }, [])

  const handleMiniModeCharChange = async (name: string) => {
    setMiniModeChar(name)
    const store = await getStore()
    await store.set('mini_character', name)
    await store.save()
  }

  return (
    <div className="max-w-7xl mx-auto space-y-10 pb-12">
      {/* Mini mode character selector */}
      {characters.length > 0 && (
        <section className="bg-white rounded-xl border border-gray-200 p-5 shadow-sm">
          <div className="flex items-center justify-between">
            <div>
              <h2 className="text-sm font-semibold text-gray-900">{t('character.miniModeChar')}</h2>
              <p className="text-xs text-gray-500 mt-1">{t('character.miniModeCharDesc')}</p>
            </div>
            <MiniCharSelect
              characters={characters}
              value={miniModeChar}
              onChange={handleMiniModeCharChange}
            />
          </div>
        </section>
      )}

      <section>
        <div className="flex items-center justify-between mb-6">
          <h2 className="text-base font-semibold text-gray-900">{t('character.characters')}</h2>
          <button
            onClick={() => setIsEditing(!isEditing)}
            className={`flex items-center gap-1.5 text-xs font-medium px-3 py-1.5 rounded-md transition-colors ${
              isEditing
                ? 'bg-gray-900 text-white hover:bg-gray-800'
                : 'bg-white border border-gray-200 text-gray-600 hover:bg-gray-50'
            }`}
          >
            {isEditing ? <>{t('common.done')}</> : <><Edit2 size={12} /> {t('character.editChar')}</>}
          </button>
        </div>
        {characters.length === 0 && <div className="text-gray-500 text-sm mb-6">{t('character.noCharacters')}</div>}
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4 gap-6">
          {characters.map((c) => (
            <MiniCharacterCard
              key={c.name}
              character={c}
              isEditing={isEditing}
              onDeleteGif={onDeleteMiniGif}
              onDeleteCharacter={() => onDelete(c.name)}
            />
          ))}
        </div>
      </section>

      {/* Upload Mini GIF */}
      <section>
        <h2 className="text-base font-semibold text-gray-900 mb-4">{t('character.uploadMiniGif')}</h2>
        <div className="bg-white rounded-xl border border-gray-200 shadow-sm p-6">
          <div className="space-y-6">
            <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
              <div>
                <label className="block text-sm font-medium text-gray-700 mb-2">{t('character.charName')}</label>
                <input
                  type="text"
                  value={miniCharName}
                  onChange={(e) => setMiniCharName(e.target.value)}
                  placeholder={t('character.actionTypePlaceholder')}
                  className="w-full bg-gray-50 border border-gray-200 text-gray-900 text-sm rounded-lg focus:ring-2 focus:ring-gray-900/10 focus:border-gray-900 block p-2.5 outline-none transition-all placeholder:text-gray-400"
                />
              </div>
              <div>
                <label className="block text-sm font-medium text-gray-700 mb-2">{t('character.actionType')}</label>
                <div className="relative">
                  <select
                    value={miniCategory}
                    onChange={(e) => setMiniCategory(e.target.value)}
                    className="w-full appearance-none bg-gray-50 border border-gray-200 text-gray-700 text-sm rounded-lg focus:ring-2 focus:ring-gray-900/10 focus:border-gray-900 block p-2.5 pr-8 outline-none transition-all cursor-pointer"
                  >
                    {MINI_CATEGORIES.map((c) => <option key={c} value={c}>{c}</option>)}
                  </select>
                  <div className="pointer-events-none absolute inset-y-0 right-0 flex items-center px-3 text-gray-400">
                    <ChevronDown size={16} />
                  </div>
                </div>
              </div>
            </div>

            <div>
              <input ref={miniInputRef} type="file" accept=".gif" multiple className="hidden" onChange={(e) => onMiniFiles(e.target.files)} />
              <UploadZoneSimple label={t('character.selectGifFile')} onClick={() => miniInputRef.current?.click()} previews={miniPreviews} />
            </div>

            <div className="pt-2">
              <button
                onClick={onMiniUpload}
                disabled={miniSaving || !miniCharName.trim() || miniPreviews.length === 0}
                className="bg-gray-900 hover:bg-gray-800 text-white text-sm font-medium py-2.5 px-6 rounded-lg transition-colors shadow-sm disabled:opacity-50 disabled:cursor-not-allowed"
              >
                {miniSaving ? t('common.saving') : t('common.upload')}
              </button>
            </div>
          </div>
        </div>
      </section>
    </div>
  )
}

// ─── Mini Character Card ───

function MiniCharacterCard({ character, isEditing, onDeleteGif, onDeleteCharacter }: {
  character: CharacterMeta
  isEditing: boolean
  onDeleteGif: (charName: string, cat: string, gifPath: string) => void
  onDeleteCharacter: () => void
}) {
  const { t } = useTranslation()
  return (
    <div className="bg-white rounded-xl border border-gray-200 shadow-sm overflow-hidden relative group">
      {isEditing && !character.builtin && (
        <button
          onClick={() => onDeleteCharacter()}
          className="absolute top-3 right-3 text-gray-400 hover:text-red-500 z-10 transition-colors"
          title={t('character.deleteChar')}
        >
          <X size={16} strokeWidth={2.5} />
        </button>
      )}
      <div className="px-5 py-3 border-b border-gray-100 bg-gray-50/50">
        <h3 className="text-sm font-semibold text-gray-900">{character.name}</h3>
      </div>
      <div className="p-5 space-y-5">
        {character.miniActions && Object.entries(character.miniActions).map(([cat, gifs]) => (
          <MiniStateGroup
            key={cat}
            title={cat}
            gifs={gifs}
            isEditing={isEditing}
            onDelete={(gifPath) => onDeleteGif(character.name, cat, gifPath)}
            allowDelete={cat !== 'walk'}
          />
        ))}
        {(!character.miniActions || Object.keys(character.miniActions).length === 0) && (
          <div className="text-sm text-gray-400 text-center py-4">{t('character.noImages')}</div>
        )}
      </div>
    </div>
  )
}

// ─── Mini State Group ───

function MiniStateGroup({ title, gifs, isEditing, onDelete, allowDelete }: {
  title: string; gifs: string[]; isEditing: boolean; onDelete: (gifPath: string) => void; allowDelete: boolean
}) {
  const { t } = useTranslation()
  if (gifs.length === 0) return null
  return (
    <div>
      <div className="flex items-center gap-2 mb-2">
        <h4 className="text-xs font-medium text-gray-500 capitalize">{title}</h4>
        <span className="text-[10px] font-medium bg-gray-100 text-gray-400 px-1.5 py-0.5 rounded">{gifs.length}</span>
      </div>
      <div className="flex flex-wrap gap-2">
        {gifs.map((g, i) => (
          <div key={i} className="relative group/img">
            <div className={`w-10 h-10 rounded-lg border flex items-center justify-center overflow-hidden transition-colors ${
              isEditing && allowDelete ? 'border-red-200 bg-red-50/30' : 'border-gray-200 bg-gray-50 group-hover/img:border-gray-300'
            }`}>
              {g && <img src={g} alt="" className="w-8 h-8 object-contain" draggable={false} />}
            </div>
            {isEditing && allowDelete && (
              <button
                onClick={() => onDelete(g)}
                className="absolute -top-1.5 -right-1.5 bg-red-500 text-white border border-white rounded-full p-0.5 shadow-sm z-10 hover:scale-110 hover:bg-red-600 transition-all"
                title={t('common.delete')}
              >
                <X size={10} strokeWidth={3} />
              </button>
            )}
          </div>
        ))}
      </div>
    </div>
  )
}

// ─── Upload Zone (with hidden file input) ───

function UploadZone({ label, inputRef, previews, onFiles }: {
  label: string
  inputRef: React.RefObject<HTMLInputElement | null>
  previews: string[]
  onFiles: (files: FileList | null) => void
}) {
  const { t } = useTranslation()
  return (
    <div>
      <label className="block text-sm font-medium text-gray-700 mb-2">{label}</label>
      <input ref={inputRef} type="file" accept=".gif" multiple className="hidden" onChange={(e) => onFiles(e.target.files)} />
      <div
        onClick={() => inputRef.current?.click()}
        className="border-2 border-dashed border-gray-200 rounded-xl p-8 flex flex-col items-center justify-center bg-gray-50/50 hover:bg-gray-50 transition-colors cursor-pointer group"
      >
        {previews.length > 0 ? (
          <div className="flex flex-wrap gap-3 justify-center">
            {previews.map((src, i) => (
              <img key={i} src={src} alt="" className="w-12 h-12 object-cover rounded-md border border-gray-200" />
            ))}
          </div>
        ) : (
          <>
            <div className="p-3 bg-white rounded-full shadow-sm border border-gray-100 mb-3 group-hover:scale-105 transition-transform">
              <UploadCloud size={20} className="text-gray-400 group-hover:text-gray-600" />
            </div>
            <span className="text-sm text-gray-600 font-medium">{t('character.clickOrDrag')}</span>
            <span className="text-xs text-gray-400 mt-1">{t('character.gifFormat')}</span>
          </>
        )}
      </div>
    </div>
  )
}

// ─── Simple Upload Zone (for mini) ───

function UploadZoneSimple({ label, onClick, previews }: {
  label: string; onClick: () => void; previews: string[]
}) {
  const { t } = useTranslation()
  return (
    <div>
      <label className="block text-sm font-medium text-gray-700 mb-2">{label}</label>
      <div
        onClick={onClick}
        className="border-2 border-dashed border-gray-200 rounded-xl p-8 flex flex-col items-center justify-center bg-gray-50/50 hover:bg-gray-50 transition-colors cursor-pointer group"
      >
        {previews.length > 0 ? (
          <div className="flex flex-wrap gap-3 justify-center">
            {previews.map((src, i) => (
              <img key={i} src={src} alt="" className="w-12 h-12 object-cover rounded-md border border-gray-200" />
            ))}
          </div>
        ) : (
          <>
            <div className="p-3 bg-white rounded-full shadow-sm border border-gray-100 mb-3 group-hover:scale-105 transition-transform">
              <UploadCloud size={20} className="text-gray-400 group-hover:text-gray-600" />
            </div>
            <span className="text-sm text-gray-600 font-medium">{t('character.clickOrDrag')}</span>
            <span className="text-xs text-gray-400 mt-1">{t('character.gifFormat')}</span>
          </>
        )}
      </div>
    </div>
  )
}

// ─── Mini Character Select Dropdown ───

function MiniCharSelect({ characters, value, onChange }: {
  characters: CharacterMeta[]
  value: string
  onChange: (name: string) => void
}) {
  const { t } = useTranslation()
  const [open, setOpen] = useState(false)
  const containerRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const onClickOutside = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('mousedown', onClickOutside)
    return () => document.removeEventListener('mousedown', onClickOutside)
  }, [])

  const getPreviewGif = (c: CharacterMeta) => {
    if (!c.miniActions) return undefined
    const all = Object.values(c.miniActions).flat()
    return all.find((g) => g.includes('idle')) || all[0]
  }

  const options = [
    { name: '', label: t('character.autoSelect'), gif: undefined as string | undefined },
    ...characters.map((c) => ({ name: c.name, label: c.name, gif: getPreviewGif(c) })),
  ]
  const selected = options.find((o) => o.name === value) || options[0]

  return (
    <div ref={containerRef} className="relative w-56">
      <button
        onClick={() => setOpen(!open)}
        className="w-full flex items-center justify-between bg-gray-50 border border-gray-200 rounded-lg px-3 py-2.5 text-sm text-left cursor-pointer hover:bg-gray-100 transition-colors focus:outline-none focus:ring-2 focus:ring-gray-900/10 focus:border-gray-900"
      >
        <div className="flex items-center gap-2 min-w-0">
          {selected.gif && <img src={selected.gif} alt="" className="w-6 h-6 object-contain shrink-0" draggable={false} />}
          <span className="text-gray-900 font-medium truncate">{selected.label}</span>
        </div>
        <ChevronDown size={16} className={`text-gray-400 shrink-0 ml-2 transition-transform ${open ? 'rotate-180' : ''}`} />
      </button>

      {open && (
        <div className="absolute top-full left-0 right-0 mt-1.5 bg-white border border-gray-200 rounded-xl shadow-lg z-20 py-1.5 max-h-60 overflow-y-auto">
          {options.map((opt) => {
            const isSelected = opt.name === value
            return (
              <button
                key={opt.name}
                onClick={() => { onChange(opt.name); setOpen(false) }}
                className={`w-full flex items-center gap-2.5 px-3 py-2 text-left text-sm transition-colors ${
                  isSelected ? 'bg-gray-50 text-gray-900' : 'text-gray-700 hover:bg-gray-50'
                }`}
              >
                {opt.gif
                  ? <img src={opt.gif} alt="" className="w-6 h-6 object-contain shrink-0" draggable={false} />
                  : <span className="w-6 text-center text-gray-400 shrink-0">-</span>
                }
                <span className={`flex-1 truncate ${isSelected ? 'font-medium' : ''}`}>{opt.label}</span>
                {isSelected && <Check size={14} className="text-emerald-500 shrink-0" />}
              </button>
            )
          })}
        </div>
      )}
    </div>
  )
}

// ─── Agent Select Dropdown ───

function AgentSelect({ agents, value, onChange }: {
  agents: AgentInfo[]
  value: string
  onChange: (agentId: string) => void
}) {
  const { t } = useTranslation()
  const [open, setOpen] = useState(false)
  const containerRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const onClickOutside = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('mousedown', onClickOutside)
    return () => document.removeEventListener('mousedown', onClickOutside)
  }, [])

  const allOptions = [
    { id: 'main', emoji: '', name: 'main', label: t('common.default') },
    ...agents.filter((a) => a.id !== 'main').map((a) => ({
      id: a.id,
      emoji: a.identityEmoji || '',
      name: a.identityName || a.id,
      label: '',
    })),
  ]
  const selected = allOptions.find((o) => o.id === value) || allOptions[0]

  return (
    <div ref={containerRef} className="relative w-72">
      <button
        onClick={() => setOpen(!open)}
        className="w-full flex items-center justify-between bg-gray-50 border border-gray-200 rounded-lg px-3 py-2.5 text-sm text-left cursor-pointer hover:bg-gray-100 transition-colors focus:outline-none focus:ring-2 focus:ring-gray-900/10 focus:border-gray-900"
      >
        <div className="flex items-center gap-2 min-w-0">
          {selected.emoji && <span className="text-base shrink-0">{selected.emoji}</span>}
          <span className="text-gray-900 font-medium truncate">{selected.name}</span>
          {selected.label && <span className="text-gray-400 text-xs shrink-0">({selected.label})</span>}
        </div>
        <ChevronDown size={16} className={`text-gray-400 shrink-0 ml-2 transition-transform ${open ? 'rotate-180' : ''}`} />
      </button>

      {open && (
        <div className="absolute top-full left-0 right-0 mt-1.5 bg-white border border-gray-200 rounded-xl shadow-lg z-20 py-1.5 max-h-60 overflow-y-auto">
          {allOptions.map((opt) => {
            const isSelected = opt.id === value
            return (
              <button
                key={opt.id}
                onClick={() => { onChange(opt.id); setOpen(false) }}
                className={`w-full flex items-center gap-2.5 px-3 py-2 text-left text-sm transition-colors ${
                  isSelected ? 'bg-gray-50 text-gray-900' : 'text-gray-700 hover:bg-gray-50'
                }`}
              >
                <span className="w-5 text-center text-base shrink-0">{opt.emoji || '🤖'}</span>
                <span className={`flex-1 truncate ${isSelected ? 'font-medium' : ''}`}>{opt.name}</span>
                {opt.label && <span className="text-gray-400 text-xs shrink-0">{opt.label}</span>}
                {isSelected && <Check size={14} className="text-emerald-500 shrink-0" />}
              </button>
            )
          })}
        </div>
      )}
    </div>
  )
}
