import { invoke } from '@tauri-apps/api/core'
import VERTC, { MediaType, RoomProfileType } from '@volcengine/rtc'

export interface RTCVoiceConfig {
  rtcAppId: string
  rtcAppKey: string
  accessKeyId: string
  secretAccessKey: string
  asrAppId: string
  asrAccessToken: string
  ttsAppId: string
  ttsAccessToken: string
  llmEndpointId: string
  llmApiKey: string
  characterPrompt: string
}

// Forward JS logs to Rust terminal (visible in `tauri dev` output)
function rlog(level: 'info' | 'warn' | 'error', msg: string) {
  console[level]('[VoiceRTC]', msg)
  invoke('js_log', { level, msg: `[VoiceRTC] ${msg}` }).catch(() => {})
}

// Prefer concrete headset devices over abstract "default/communications" aliases.
const PREFERRED_HEADSET_LABELS = [
  'cx plus true wireless hands-free ag audio',
  'hands-free ag audio',
]

export class VoiceRTCClient {
  private config: RTCVoiceConfig
  private engine: any = null
  private roomId: string
  private userId: string
  private active: boolean = false
  private micCapturing: boolean = false
  private endingSpeechTurn: boolean = false
  private micDeviceId: string | undefined
  private audioContext: AudioContext | null = null  // shared, user-gesture-unlocked AC

  constructor(config: RTCVoiceConfig) {
    this.config = config
    // Standard room/user IDs for Web RTC AIGC
    const ts = Date.now()
    this.roomId = `oc_${ts}`
    this.userId = `u_${ts}`
  }

  async startVoiceChat(): Promise<void> {
    if (this.active) {
      await this.resumeUserSpeech()
      return
    }

    try {
      rlog('info', `starting — roomId=${this.roomId} userId=${this.userId}`)

      // Unlock AudioContext on user gesture (required by WebView2 autoplay policy)
      // Also route output to communications/headphone device (skip Voicemeeter)
      try {
        const ac = new AudioContext()
        if (ac.state === 'suspended') await ac.resume()
        const buf = ac.createBuffer(1, 1, 22050)
        const src = ac.createBufferSource()
        src.buffer = buf
        src.connect(ac.destination)
        src.start(0)
        this.audioContext = ac
        rlog('info', `AudioContext state: ${ac.state}`)

        // Try to route output to the communications/headphone device (not Voicemeeter)
        try {
          const allDevices = await navigator.mediaDevices.enumerateDevices()
          const outputs = allDevices.filter(d => d.kind === 'audiooutput')
          rlog('info', `audio outputs (${outputs.length}): ${outputs.map(d => `[${d.deviceId.slice(0,8)}] ${d.label}`).join(' | ')}`)
          const SKIP_OUT = ['voicemeeter', 'vb-audio', 'virtual', 'steam']
          const isRealOut = (d: MediaDeviceInfo) => !SKIP_OUT.some(s => d.label.toLowerCase().includes(s))
          const preferredOut = outputs.find(d =>
            d.deviceId !== 'default' &&
            d.deviceId !== 'communications' &&
            isRealOut(d) &&
            PREFERRED_HEADSET_LABELS.some(k => d.label.toLowerCase().includes(k))
          )
          const realOut = outputs.find(d => d.deviceId !== 'default' && d.deviceId !== 'communications' && isRealOut(d))
          const commsOut = outputs.find(d => d.deviceId === 'communications' && isRealOut(d))
          const pickOut = preferredOut || realOut || commsOut
          if (pickOut && (ac as any).setSinkId) {
            await (ac as any).setSinkId(pickOut.deviceId)
            rlog('info', `AudioContext output set to: [${pickOut.deviceId.slice(0,8)}] ${pickOut.label}`)
          } else if (pickOut) {
            rlog('warn', `setSinkId not supported, output device: [${pickOut.deviceId.slice(0,8)}] ${pickOut.label}`)
          }
        } catch (e: any) {
          rlog('warn', `output device selection failed: ${e?.message}`)
        }
      } catch (e) {
        rlog('warn', `AudioContext unlock failed: ${e}`)
      }

      // Enumerate audio devices to find a real microphone
      try {
        const devices = await navigator.mediaDevices.enumerateDevices()
        const audioInputs = devices.filter(d => d.kind === 'audioinput')
        rlog('info', `audio inputs (${audioInputs.length}): ${audioInputs.map(d => `[${d.deviceId.slice(0,8)}] ${d.label}`).join(' | ')}`)
      } catch (e: any) {
        rlog('warn', `enumerateDevices failed: ${e?.message}`)
      }

      // Quick mic sanity check — detect if mic is real or fake/silent
      try {
        const testStream = await navigator.mediaDevices.getUserMedia({ audio: true })
        const track = testStream.getAudioTracks()[0]
        rlog('info', `mic track: label="${track?.label}" enabled=${track?.enabled} readyState=${track?.readyState}`)
        const testAC = new AudioContext()
        await testAC.resume()
        const src2 = testAC.createMediaStreamSource(testStream)
        const analyser = testAC.createAnalyser()
        src2.connect(analyser)
        // Wait longer and check multiple times
        let maxRms = 0
        for (let i = 0; i < 6; i++) {
          await new Promise(r => setTimeout(r, 300))
          const data = new Float32Array(analyser.fftSize)
          analyser.getFloatTimeDomainData(data)
          const rms = Math.sqrt(data.reduce((s, v) => s + v * v, 0) / data.length)
          if (rms > maxRms) maxRms = rms
        }
        rlog('info', `mic test maxRMS=${maxRms.toFixed(6)} ${maxRms > 0.001 ? '✅ REAL mic (has signal)' : '⚠️ SILENT mic (fake or muted)'}`)
        testStream.getTracks().forEach(t => t.stop())
        testAC.close()
      } catch (err: any) {
        rlog('error', `mic test failed: ${err?.message} — mic permission may be denied`)
      }

      // 1. Generate RTC token via Rust
      const token = await invoke<string>('generate_rtc_token', {
        appId: this.config.rtcAppId,
        appKey: this.config.rtcAppKey,
        roomId: this.roomId,
        userId: this.userId,
        accessKeyId: this.config.accessKeyId,
        secretAccessKey: this.config.secretAccessKey,
      })
      rlog('info', 'token generated')

      // 2. Create engine
      // Intercept RTCPeerConnection at the lowest level to capture VERTC's internal audio tracks
      // This is needed because VERTC manages audio internally without exposing the track to DOM
      const self = this  // capture for closure
      if (!(window as any).__rtcPCPatched) {
        (window as any).__rtcPCPatched = true
        const OrigPC = window.RTCPeerConnection
        ;(window as any).RTCPeerConnection = new Proxy(OrigPC, {
          construct(target: any, args: any[]) {
            const pc = new target(...args)
            pc.addEventListener('track', (event: RTCTrackEvent) => {
              if (event.track.kind === 'audio') {
                rlog('info', `RTCPeerConnection track event: audio track captured`)
                const ac = self.audioContext
                if (!ac) { rlog('warn', 'no shared AudioContext available'); return }
                try {
                  const stream = event.streams[0] ?? new MediaStream([event.track])
                  const source = ac.createMediaStreamSource(stream)
                  source.connect(ac.destination)
                  rlog('info', `audio track routed → AC state=${ac.state}`)
                } catch (err: any) {
                  rlog('error', `track routing error: ${err?.message}`)
                }
              }
            })
            return pc
          }
        })
        rlog('info', 'RTCPeerConnection track interceptor installed')
      } else {
        // Already patched — update the closure reference so new client uses new AC
        ;(window as any).__rtcSelf = this
        rlog('info', 'RTCPeerConnection interceptor already installed, updated self ref')
      }

      rlog('info', `createEngine appId=${this.config.rtcAppId}`)
      this.engine = VERTC.createEngine(this.config.rtcAppId)

      // Handle VERTC internal autoplay failure — must resume on user gesture
      this.engine.on(VERTC.events.onAutoplayFailed, async (e: any) => {
        rlog('warn', `onAutoplayFailed: ${JSON.stringify(e)}`)
      })

      // Monitor local mic level — confirms our voice is actually being sent
      this.engine.on(VERTC.events.onLocalAudioPropertiesReport, (infos: any[]) => {
        if (infos && infos.length > 0) {
          const level = infos[0]?.audioPropertiesInfo?.linearVolume ?? '?'
          if (level !== '?' && level > 0) {
            rlog('info', `local mic level: ${level}`)
          }
        }
      })

      // Fallback: check if local event name differs
      const localEvt = Object.keys(VERTC.events).find(k => k.toLowerCase().includes('local') && k.toLowerCase().includes('audio'))
      rlog('info', `local audio event name: ${localEvt} = ${(VERTC.events as any)[localEvt ?? '']}`)

      // Enable audio level reporting (remote + local)
      this.engine.enableAudioPropertiesReport({ interval: 500, includeLocalUser: true, smooth: false })
      // Some VERTC versions have a separate method for local reporting
      try { this.engine.enableLocalAudioPropertiesReport({ interval: 500 }) } catch (_) {}

      this.engine.on(VERTC.events.onRemoteAudioPropertiesReport, (infos: any[]) => {
        if (infos && infos.length > 0) {
          // Log full structure once to understand the schema
          if (!(window as any).__audioLevelLogged) {
            (window as any).__audioLevelLogged = true
            rlog('info', `audio report schema: ${JSON.stringify(infos[0])}`)
          }
          const levels = infos.map((i: any) => {
            const uid = i.streamKey?.userId ?? '?'
            const level = i.audioPropertiesInfo?.linearVolume ?? '?'
            return `${uid}:${level}`
          }).join(', ')
          if (!levels.split(',').every(s => s.includes(':0'))) {
            rlog('info', `remote audio levels: ${levels}`)
          }
        }
      })

      // Also listen for local audio properties (same handler in some SDK versions)
      this.engine.on(VERTC.events.onLocalAudioPropertiesReport, (infos: any[]) => {
        if (infos && infos.length > 0) {
          const level = infos[0]?.audioPropertiesInfo?.linearVolume ?? 0
          if (level > 5) rlog('info', `local mic level: ${level} ✅ voice being sent`)
        }
      })

      // 3. Listen for bot joining (AI ready)
      this.engine.on(VERTC.events.onUserJoined, (e: any) => {
        rlog('info', `user joined: ${e.userInfo?.userId}`)
      })

      // 4. Auto-subscribe remote audio (AI voice) + attach to <audio> element
      this.engine.on(VERTC.events.onUserPublishStream, async (e: any) => {
        rlog('info', `onUserPublishStream userId=${e.userId} mediaType=${e.mediaType}`)
        if (e.mediaType === MediaType.AUDIO) {
          await this.engine.subscribeStream(e.userId, MediaType.AUDIO)
          rlog('info', `subscribed to audio from ${e.userId}`)

          // Attach remote audio to DOM element and play
          try {
            let audioEl = document.getElementById('rtc-remote-audio') as HTMLAudioElement
            if (!audioEl) {
              audioEl = document.createElement('audio')
              audioEl.id = 'rtc-remote-audio'
              audioEl.autoplay = true
              audioEl.style.cssText = 'position:fixed;left:-9999px;top:-9999px;width:1px;height:1px;'
              document.body.appendChild(audioEl)
            }
            // VERTC play(userId, domElement) — routes audio stream to the element
            await this.engine.play(e.userId, audioEl)
            rlog('info', `play(${e.userId}, audioEl) OK — srcObject=${audioEl.srcObject ? 'set' : 'null'} readyState=${audioEl.readyState} paused=${audioEl.paused}`)
            // Also try direct play on the element
            try { await audioEl.play(); rlog('info', 'audioEl.play() OK') } catch (pe: any) { rlog('warn', `audioEl.play() err: ${pe?.message}`) }
          } catch (err: any) {
            rlog('warn', `play with element error: ${err?.message}`)
            // Fallback: play without element
            try {
              await this.engine.play(e.userId)
              rlog('info', `play(${e.userId}) fallback OK`)
            } catch (err2: any) {
              rlog('error', `play fallback error: ${err2?.message}`)
            }
          }
        }
      })

      // 4b. When remote audio track arrives, attach to DOM <audio> element
      this.engine.on(VERTC.events.onUserStartAudioCapture, (e: any) => {
        rlog('info', `onUserStartAudioCapture userId=${e.userId}`)
      })

      // Some VERTC versions require explicit audio element setup
      this.engine.on('onTrackAdded' as any, (e: any) => {
        rlog('info', `onTrackAdded kind=${e?.track?.kind} userId=${e?.userId}`)
        if (e?.track?.kind === 'audio') {
          try {
            let audioEl = document.getElementById('rtc-remote-audio') as HTMLAudioElement
            if (!audioEl) {
              audioEl = document.createElement('audio')
              audioEl.id = 'rtc-remote-audio'
              audioEl.autoplay = true
              audioEl.style.display = 'none'
              document.body.appendChild(audioEl)
            }
            const stream = new MediaStream([e.track])
            audioEl.srcObject = stream
            audioEl.play().then(() => rlog('info', 'remote audio element playing'))
              .catch((err: any) => rlog('error', `audio element play failed: ${err?.message}`))
          } catch (err: any) {
            rlog('error', `onTrackAdded handler failed: ${err?.message}`)
          }
        }
      })

      // 5. Pre-create audio element and unlock autoplay before joining room
      {
        let audioEl = document.getElementById('rtc-remote-audio') as HTMLAudioElement
        if (!audioEl) {
          audioEl = document.createElement('audio')
          audioEl.id = 'rtc-remote-audio'
          audioEl.autoplay = true
          audioEl.controls = false
          audioEl.style.cssText = 'position:fixed;left:-9999px;top:-9999px;width:1px;height:1px;'
          // Silent MP3 data URI to unlock autoplay
          audioEl.src = 'data:audio/mpeg;base64,/+MYxAAAAANIAAAAAExBTUUzLjk4LjIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'
          document.body.appendChild(audioEl)
          try { await audioEl.play() } catch (_) {}
          audioEl.src = ''
          rlog('info', 'pre-created audio element and unlocked autoplay')
        }
      }

      // 6. Join room
      rlog('info', `joinRoom roomId=${this.roomId}`)
      await this.engine.joinRoom(
        token,
        this.roomId,
        {
          userId: this.userId,
          extraInfo: JSON.stringify({
            call_scene: 'RTC-AIGC',
            user_name: this.userId,
            user_id: this.userId,
          }),
        },
        {
          isAutoPublish: true,
          isAutoSubscribeAudio: true,
          roomProfileType: RoomProfileType.chat,
        }
      )
      rlog('info', 'joined room OK')

      // 6. Select best microphone (skip virtual/Voicemeeter devices, prefer communications device)
      try {
        const devices = await navigator.mediaDevices.enumerateDevices()
        const inputs = devices.filter(d => d.kind === 'audioinput')
        // Priority: preferred headset label > real mic > communications alias
        const SKIP_LABELS = ['voicemeeter', 'steam streaming', 'vb-audio', 'virtual']
        const isReal = (d: MediaDeviceInfo) => !SKIP_LABELS.some(s => d.label.toLowerCase().includes(s))
        const preferredDev = inputs.find(d =>
          d.deviceId !== 'default' &&
          d.deviceId !== 'communications' &&
          isReal(d) &&
          PREFERRED_HEADSET_LABELS.some(k => d.label.toLowerCase().includes(k))
        )
        const realDev = inputs.find(d => d.deviceId !== 'default' && d.deviceId !== 'communications' && isReal(d))
        const commsDev = inputs.find(d => d.deviceId === 'communications' && isReal(d))
        const pick = preferredDev || realDev || commsDev
        if (pick) {
          this.micDeviceId = pick.deviceId
          rlog('info', `selected mic: [${pick.deviceId.slice(0,8)}] ${pick.label}`)
        } else {
          rlog('warn', 'no real mic found, using default (may be silent)')
        }
      } catch (e: any) {
        rlog('warn', `device selection failed: ${e?.message}`)
      }

      await this.resumeUserSpeech()

      // 7. Call Volcano StartVoiceChat API via Rust
      rlog('info', 'calling start_rtc_voice_chat...')
      await invoke('start_rtc_voice_chat', {
        appId: this.config.rtcAppId,
        accessKeyId: this.config.accessKeyId,
        secretAccessKey: this.config.secretAccessKey,
        roomId: this.roomId,
        userId: this.userId,
        asrAppId: this.config.asrAppId,
        asrAccessToken: this.config.asrAccessToken,
        ttsAppId: this.config.ttsAppId,
        ttsAccessToken: this.config.ttsAccessToken,
        llmEndpointId: this.config.llmEndpointId,
        llmApiKey: this.config.llmApiKey,
        characterPrompt: this.config.characterPrompt || '你是一只可爱的桌面宠物，陪伴用户玩游戏和工作，回答简短活泼',
      })
      rlog('info', 'StartVoiceChat OK — voice chat active!')

      this.active = true
    } catch (error: any) {
      rlog('error', `start failed: ${error?.message ?? String(error)}`)
      await this._cleanup()
      throw error
    }
  }

  async stopVoiceChat(): Promise<void> {
    if (!this.active) return

    try {
      await invoke('stop_rtc_voice_chat', {
        appId: this.config.rtcAppId,
        accessKeyId: this.config.accessKeyId,
        secretAccessKey: this.config.secretAccessKey,
        roomId: this.roomId,
      })
    } catch (e: any) {
      rlog('warn', `stop API error: ${e?.message ?? String(e)}`)
    }

    await this._cleanup()
  }

  async endUserSpeechTurn(): Promise<void> {
    if (!this.active || !this.engine) return
    if (!this.micCapturing) {
      rlog('info', 'endUserSpeechTurn skipped: mic not capturing')
      return
    }
    if (this.endingSpeechTurn) {
      rlog('info', 'endUserSpeechTurn skipped: already ending')
      return
    }
    this.endingSpeechTurn = true

    try {
      // Keep a short tail window so server-side VAD can receive trailing silence.
      await new Promise(r => setTimeout(r, 700))
      await this.engine.stopAudioCapture()
      this.micCapturing = false
      rlog('info', 'mic paused — waiting AI response')
    } catch (e: any) {
      rlog('warn', `stopAudioCapture failed: ${e?.message ?? String(e)}`)
    }

    // Force ASR finalize on push-to-talk release so AI can reply even with noisy input.
    try {
      await invoke('update_rtc_voice_chat', {
        appId: this.config.rtcAppId,
        accessKeyId: this.config.accessKeyId,
        secretAccessKey: this.config.secretAccessKey,
        roomId: this.roomId,
        command: 'FinishSpeechRecognition',
      })
      rlog('info', 'FinishSpeechRecognition sent')
    } catch (e: any) {
      rlog('warn', `FinishSpeechRecognition failed: ${e?.message ?? String(e)}`)
    } finally {
      this.endingSpeechTurn = false
    }
  }

  async resumeUserSpeech(): Promise<void> {
    if (!this.engine || this.micCapturing) return
    this.endingSpeechTurn = false

    rlog('info', 'startAudioCapture...')
    if (this.micDeviceId) {
      await this.engine.startAudioCapture(this.micDeviceId)
    } else {
      await this.engine.startAudioCapture()
    }
    this.micCapturing = true
    rlog('info', 'mic started')

    // Explicitly publish audio stream (isAutoPublish may not work in all VERTC versions)
    try {
      await this.engine.publishStream(MediaType.AUDIO)
      rlog('info', 'publishStream(AUDIO) OK — mic stream published to room')
    } catch (err: any) {
      rlog('warn', `publishStream: ${err?.message} (may already be auto-published)`)
    }
  }

  private async _cleanup() {
    if (this.engine) {
      try {
        await this.engine.stopAudioCapture()
        await this.engine.leaveRoom()
        VERTC.destroyEngine(this.engine)
      } catch (e: any) {
        rlog('warn', `cleanup error: ${e?.message ?? String(e)}`)
      }
      this.engine = null
    }
    this.active = false
    this.micCapturing = false
    this.endingSpeechTurn = false
    this.micDeviceId = undefined
    rlog('info', 'cleaned up')
  }

  isActive(): boolean {
    return this.active
  }
}
