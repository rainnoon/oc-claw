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

export class VoiceRTCClient {
  private config: RTCVoiceConfig
  private engine: any = null
  private roomId: string
  private userId: string
  private active: boolean = false
  private audioContext: AudioContext | null = null  // shared, user-gesture-unlocked AC

  constructor(config: RTCVoiceConfig) {
    this.config = config
    this.roomId = `oc_${Date.now()}`
    this.userId = `u_${Date.now()}`
  }

  async startVoiceChat(): Promise<void> {
    if (this.active) return

    try {
      rlog('info', `starting — roomId=${this.roomId} userId=${this.userId}`)

      // Unlock AudioContext on user gesture (required by WebView2 autoplay policy)
      try {
        const ac = new AudioContext()
        if (ac.state === 'suspended') await ac.resume()
        // Play a silent buffer to fully unlock audio
        const buf = ac.createBuffer(1, 1, 22050)
        const src = ac.createBufferSource()
        src.buffer = buf
        src.connect(ac.destination)
        src.start(0)
        this.audioContext = ac  // save for later use in track routing
        rlog('info', `AudioContext state: ${ac.state}`)
      } catch (e) {
        rlog('warn', `AudioContext unlock failed: ${e}`)
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
        { userId: this.userId },
        {
          isAutoPublish: true,
          isAutoSubscribeAudio: true,
          roomProfileType: RoomProfileType.chat,
        }
      )
      rlog('info', 'joined room OK')

      // 6. Start mic
      rlog('info', 'startAudioCapture...')
      await this.engine.startAudioCapture()
      rlog('info', 'mic started')

      // Explicitly publish audio stream (isAutoPublish may not work in all VERTC versions)
      try {
        await this.engine.publishStream(MediaType.AUDIO)
        rlog('info', 'publishStream(AUDIO) OK — mic stream published to room')
      } catch (err: any) {
        rlog('warn', `publishStream: ${err?.message} (may already be auto-published)`)
      }

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
    rlog('info', 'cleaned up')
  }

  isActive(): boolean {
    return this.active
  }
}
