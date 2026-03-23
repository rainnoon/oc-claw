import { useState, useEffect } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { Loader2, Check, ChevronDown, Copy } from 'lucide-react'
import { AnimatePresence, motion } from 'motion/react'
import { getStore } from '../lib/store'

function Toggle({ checked, onChange }: { checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <button
      onClick={() => onChange(!checked)}
      className={`relative inline-flex h-6 w-11 shrink-0 cursor-pointer rounded-full border-2 border-transparent transition-colors duration-200 ease-in-out focus:outline-none ${checked ? 'bg-blue-500' : 'bg-white/10'}`}
      role="switch"
      aria-checked={checked}
    >
      <span
        className={`pointer-events-none inline-block h-5 w-5 transform rounded-full bg-white shadow ring-0 transition duration-200 ease-in-out ${checked ? 'translate-x-5' : 'translate-x-0'}`}
      />
    </button>
  )
}

function CopyCode({ text }: { text: string }) {
  const [copied, setCopied] = useState(false)
  return (
    <div className="flex items-center gap-1 bg-black/40 rounded overflow-hidden">
      <code className="flex-1 px-2 py-1 text-[11px] text-white/60 font-mono select-all">{text}</code>
      <button
        onClick={() => { navigator.clipboard.writeText(text); setCopied(true); setTimeout(() => setCopied(false), 1500) }}
        className="px-1.5 py-1 text-white/30 hover:text-white/60 transition-colors shrink-0"
      >
        {copied ? <Check className="w-3 h-3 text-emerald-400" /> : <Copy className="w-3 h-3" />}
      </button>
    </div>
  )
}

export function SettingsTab({ showWorkDetail, onToggleWorkDetail }: { showWorkDetail: boolean; onToggleWorkDetail: (v: boolean) => void }) {
  const [ocMode, setOcMode] = useState<'local' | 'remote'>('local')
  const [sshHost, setSshHost] = useState('')
  const [sshUser, setSshUser] = useState('')
  const [enableOpenClaw, setEnableOpenClaw] = useState(true)
  const [enableClaudeCode, setEnableClaudeCode] = useState(true)
  const [hookStatus, setHookStatus] = useState('')
  const [showGuide, setShowGuide] = useState(false)
  const [isTesting, setIsTesting] = useState(false)
  const [testResult, setTestResult] = useState<'success' | 'error' | null>(null)
  const [testMsg, setTestMsg] = useState('')
  const [updateInfo, setUpdateInfo] = useState<{ current: string; latest: string; hasUpdate: boolean; url: string } | null>(null)
  const [updateChecking, setUpdateChecking] = useState(false)
  const [updating, setUpdating] = useState(false)

  useEffect(() => {
    ;(async () => {
      const store = await getStore()
      const m = (await store.get('oc_mode')) as string
      if (m === 'remote') setOcMode('remote')
      setSshHost(((await store.get('ssh_host')) as string) || '')
      setSshUser(((await store.get('ssh_user')) as string) || '')
      const oc = await store.get('enable_openclaw')
      if (typeof oc === 'boolean') setEnableOpenClaw(oc)
      const cc = await store.get('enable_claudecode')
      if (typeof cc === 'boolean') setEnableClaudeCode(cc)
    })()
    invoke('check_for_update').then((info: any) => setUpdateInfo(info)).catch(() => {})
  }, [])

  const saveSettings = async () => {
    const store = await getStore()
    await store.set('oc_mode', ocMode)
    await store.set('ssh_host', sshHost)
    await store.set('ssh_user', sshUser)
    await store.save()
  }

  const testConnection = async () => {
    setIsTesting(true)
    setTestResult(null)
    setTestMsg('')
    try {
      await saveSettings()
      if (ocMode === 'remote') {
        const result: any = await invoke('get_agents', { mode: 'remote', sshHost, sshUser })
        setTestMsg(`${result.length} 个 agent`)
      } else {
        const store = await getStore()
        const agentId = ((await store.get('tracked_agent')) as string) || 'main'
        const result: any = await invoke('get_status', { gatewayUrl: 'http://localhost:4446', token: '', agentId })
        setTestMsg(`${result.sessions.length} 个 session`)
      }
      setTestResult('success')
      setTimeout(() => setTestResult(null), 3000)
    } catch (e: any) {
      setTestResult('error')
      setTestMsg(String(e))
    }
    setIsTesting(false)
  }

  const toggleOpenClaw = async (val: boolean) => {
    setEnableOpenClaw(val)
    const store = await getStore()
    await store.set('enable_openclaw', val)
    await store.save()
  }

  const toggleClaudeCode = async (val: boolean) => {
    setEnableClaudeCode(val)
    const store = await getStore()
    await store.set('enable_claudecode', val)
    await store.save()
    if (val) {
      try {
        await invoke('install_claude_hooks')
        setHookStatus('Hook 已安装')
      } catch (e: any) {
        setHookStatus(`安装失败: ${String(e)}`)
      }
    }
  }

  return (
    <div className="max-w-2xl mx-auto pt-10 pb-20 px-6 flex flex-col gap-10">
      {/* Agent 来源 */}
      <section className="flex flex-col gap-4">
        <h2 className="text-lg font-medium text-white">Agent 来源</h2>
        <div className="bg-[#0f0f0f] border border-white/5 rounded-2xl overflow-hidden">
          <div className="flex items-center justify-between p-4 border-b border-white/5">
            <div className="flex flex-col gap-1">
              <span className="text-sm font-medium text-white/90">OpenClaw</span>
              <span className="text-xs text-white/40">连接 OpenClaw Gateway 获取 agent 状态</span>
            </div>
            <Toggle checked={enableOpenClaw} onChange={toggleOpenClaw} />
          </div>
          <div className="flex items-center justify-between p-4">
            <div className="flex flex-col gap-1">
              <span className="text-sm font-medium text-white/90">Claude Code</span>
              <span className="text-xs text-white/40">通过 Hook 监听本地 Claude Code 会话</span>
              {hookStatus && <span className="text-xs text-white/30 mt-1">{hookStatus}</span>}
            </div>
            <Toggle checked={enableClaudeCode} onChange={toggleClaudeCode} />
          </div>
        </div>
      </section>

      {/* OpenClaw 连接 */}
      <section className="flex flex-col gap-4">
        <h2 className="text-lg font-medium text-white">OpenClaw 连接</h2>
        <div className="bg-[#0f0f0f] border border-white/5 rounded-2xl p-5 flex flex-col gap-6">
          <div className="flex flex-col gap-3">
            <span className="text-sm font-medium text-white/80">连接模式</span>
            <div className="flex bg-black/50 p-1 rounded-lg border border-white/5 w-fit">
              <button
                onClick={async () => { setOcMode('local'); setTestResult(null); const store = await getStore(); await store.set('oc_mode', 'local'); await store.save(); invoke('close_ssh', {}).catch(() => {}) }}
                className={`px-6 py-1.5 text-sm font-medium rounded-md transition-colors ${ocMode === 'local' ? 'bg-white/10 text-white shadow-sm' : 'text-white/50 hover:text-white/80'}`}
              >
                本地
              </button>
              <button
                onClick={async () => { setOcMode('remote'); setTestResult(null); const store = await getStore(); await store.set('oc_mode', 'remote'); await store.save() }}
                className={`px-6 py-1.5 text-sm font-medium rounded-md transition-colors ${ocMode === 'remote' ? 'bg-white/10 text-white shadow-sm' : 'text-white/50 hover:text-white/80'}`}
              >
                远程
              </button>
            </div>
            <p className="text-xs text-white/40">
              {ocMode === 'local'
                ? '读取本机 ~/.openclaw 目录，需要本地安装 OpenClaw'
                : '通过 SSH 连接远程服务器，读取 OpenClaw 数据'}
            </p>

            <AnimatePresence>
              {ocMode === 'remote' && (
                <motion.div
                  initial={{ height: 0, opacity: 0 }}
                  animate={{ height: 'auto', opacity: 1 }}
                  exit={{ height: 0, opacity: 0 }}
                  className="flex flex-col gap-4 overflow-hidden"
                >
                  <div className="flex flex-col gap-2 pt-2">
                    <label className="text-sm text-white/80 font-medium">SSH 连接</label>
                    <div className="flex gap-2">
                      <input
                        type="text"
                        value={sshUser}
                        onChange={(e) => setSshUser(e.target.value)}
                        onBlur={saveSettings}
                        placeholder="用户名，如 root"
                        className="w-28 bg-black/50 border border-white/10 rounded-lg px-4 py-2.5 text-sm text-white placeholder:text-white/30 focus:outline-none focus:border-white/30 transition-colors"
                      />
                      <span className="self-center text-white/30 text-sm">@</span>
                      <input
                        type="text"
                        value={sshHost}
                        onChange={(e) => setSshHost(e.target.value)}
                        onBlur={saveSettings}
                        placeholder="服务器地址，如 xx.xx.xx.xx"
                        className="flex-1 bg-black/50 border border-white/10 rounded-lg px-4 py-2.5 text-sm text-white placeholder:text-white/30 focus:outline-none focus:border-white/30 transition-colors"
                      />
                    </div>
                    <button
                      onClick={() => setShowGuide(!showGuide)}
                      className="flex items-center gap-1 text-xs text-white/40 hover:text-white/60 transition-colors w-fit"
                    >
                      <ChevronDown className={`w-3 h-3 transition-transform ${showGuide ? 'rotate-0' : '-rotate-90'}`} />
                      如何连接远程服务器？
                    </button>
                    <AnimatePresence>
                      {showGuide && (
                        <motion.div
                          initial={{ height: 0, opacity: 0 }}
                          animate={{ height: 'auto', opacity: 1 }}
                          exit={{ height: 0, opacity: 0 }}
                          className="overflow-hidden"
                        >
                          <div className="bg-white/[0.03] border border-white/5 rounded-lg p-3 flex flex-col gap-2 text-xs text-white/50 leading-relaxed">
                            <p className="text-white/70 font-medium">前置条件</p>
                            <p>远程服务器需安装 OpenClaw 并运行 Gateway</p>
                            <p className="text-white/70 font-medium pt-1">步骤</p>
                            <p>1. 生成本地 SSH 密钥（如果没有）</p>
                            <CopyCode text="ssh-keygen -t ed25519" />
                            <p>2. 将公钥复制到远程服务器</p>
                            <CopyCode text={`ssh-copy-id -i ~/.ssh/id_ed25519.pub ${sshUser || '用户名'}@${sshHost || '服务器地址'}`} />
                            <p>3. 验证免密登录</p>
                            <CopyCode text={`ssh ${sshUser || '用户名'}@${sshHost || '服务器地址'} "echo ok"`} />
                            <p>4. 在上方填入用户名和服务器地址，点击「测试连接」</p>
                          </div>
                        </motion.div>
                      )}
                    </AnimatePresence>
                  </div>
                </motion.div>
              )}
            </AnimatePresence>
          </div>

          <div className="flex items-center gap-3 pt-2 border-t border-white/5">
            <button
              onClick={testConnection}
              disabled={isTesting}
              className="px-4 py-2 bg-white/5 hover:bg-white/10 border border-white/10 rounded-lg text-sm font-medium text-white transition-colors flex items-center gap-2 disabled:opacity-50"
            >
              {isTesting && <Loader2 className="w-4 h-4 animate-spin" />}
              测试连接
            </button>
            {testResult === 'success' && (
              <span className="text-sm text-emerald-400 flex items-center gap-1 ml-2">
                <Check className="w-4 h-4" /> 连接成功 {testMsg && `· ${testMsg}`}
              </span>
            )}
            {testResult === 'error' && (
              <span className="text-sm text-red-400 ml-2">
                失败: {testMsg}
              </span>
            )}
          </div>
        </div>
      </section>

      {/* 显示设置 */}
      <section className="flex flex-col gap-4">
        <h2 className="text-lg font-medium text-white">显示设置</h2>
        <div className="bg-[#0f0f0f] border border-white/5 rounded-2xl p-4">
          <div className="flex items-center justify-between">
            <div className="flex flex-col gap-1">
              <span className="text-sm font-medium text-white/90">显示工作详情</span>
              <span className="text-xs text-white/40">工作中时在 pet 上显示最新动态</span>
            </div>
            <Toggle checked={showWorkDetail} onChange={onToggleWorkDetail} />
          </div>
        </div>
      </section>

      {/* 关于 */}
      <section className="flex flex-col gap-4">
        <h2 className="text-lg font-medium text-white">关于</h2>
        <div className="bg-[#0f0f0f] border border-white/5 rounded-2xl overflow-hidden">
          <div className="flex items-center justify-between p-4">
            <div className="flex flex-col gap-1">
              <span className="text-sm font-medium text-white/90">当前版本</span>
              <span className="text-xs text-white/40">
                {updateInfo ? `v${updateInfo.current}` : '...'}
                {updateInfo && !updateInfo.hasUpdate && ' (Latest)'}
                {updateInfo?.hasUpdate && (
                  <span className="ml-2 text-emerald-400">v{updateInfo.latest} 可用</span>
                )}
              </span>
            </div>
            <div className="flex items-center gap-2">
              {updateInfo?.hasUpdate && (
                <button
                  onClick={async () => {
                    setUpdating(true)
                    try { await invoke('run_update') } catch (e: any) { setUpdating(false) }
                  }}
                  disabled={updating}
                  className="px-4 py-2 bg-blue-500 hover:bg-blue-600 text-white rounded-lg text-sm font-medium transition-colors disabled:opacity-50"
                >
                  {updating ? '更新中...' : '立即更新'}
                </button>
              )}
              <button
                onClick={async () => {
                  setUpdateChecking(true)
                  try {
                    const info = await invoke('check_for_update') as any
                    setUpdateInfo(info)
                  } catch { setUpdateInfo(null) }
                  setUpdateChecking(false)
                }}
                disabled={updateChecking}
                className="px-4 py-2 bg-white/5 hover:bg-white/10 border border-white/10 rounded-lg text-sm font-medium text-white transition-colors disabled:opacity-50"
              >
                {updateChecking ? '检查中...' : '检查更新'}
              </button>
            </div>
          </div>
        </div>
      </section>

      {/* 退出 */}
      <section className="pt-4">
        <button
          onClick={() => invoke('exit_app').catch(() => {})}
          className="w-full py-3 bg-red-500/10 hover:bg-red-500/20 border border-red-500/20 text-red-400 rounded-xl text-sm font-medium transition-colors"
        >
          退出应用
        </button>
      </section>
    </div>
  )
}
