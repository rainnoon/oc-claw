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

  constructor(config: RTCVoiceConfig) {
    this.config = config
    const ts = Date.now()
    this.roomId = `oc_${ts}`
    this.userId = `u_${ts}`
  }

  async startVoiceChat(): Promise<void> {
    if (this.active) return

    try {
      rlog('info', `starting — roomId=${this.roomId} userId=${this.userId}`)

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

      // 2. Create VERTC engine (same as official demo)
      this.engine = VERTC.createEngine(this.config.rtcAppId)
      rlog('info', `createEngine appId=${this.config.rtcAppId}`)

      // 3. Register AI noise reduction extension (optional)
      try {
        const { default: RTCAIAnsExtension } = await import('@volcengine/rtc/extension-ainr')
        const ext = new RTCAIAnsExtension()
        await this.engine.registerExtension(ext)
        ext.enable()
        rlog('info', 'AI noise reduction enabled')
      } catch (e: any) {
        rlog('warn', `AI noise reduction not available: ${e?.message}`)
      }

      // 4. Event listeners
      this.engine.on(VERTC.events.onError, (e: any) => {
        rlog('error', `engine error: ${JSON.stringify(e)}`)
      })
      this.engine.on(VERTC.events.onUserJoined, (e: any) => {
        rlog('info', `user joined: ${e.userInfo?.userId}`)
      })
      this.engine.on(VERTC.events.onAutoplayFailed, (e: any) => {
        rlog('warn', `autoplay failed: ${JSON.stringify(e)}`)
      })

      // Monitor audio levels
      this.engine.enableAudioPropertiesReport({ interval: 1000, includeLocalUser: true })
      this.engine.on(VERTC.events.onLocalAudioPropertiesReport, (infos: any[]) => {
        const level = infos?.[0]?.audioPropertiesInfo?.linearVolume ?? 0
        if (level > 5) rlog('info', `local mic level: ${level} ✅`)
      })
      this.engine.on(VERTC.events.onRemoteAudioPropertiesReport, (infos: any[]) => {
        if (!infos?.length) return
        const hasSound = infos.some((i: any) => (i.audioPropertiesInfo?.linearVolume ?? 0) > 0)
        if (hasSound) {
          const levels = infos.map((i: any) =>
            `${i.streamKey?.userId}:${i.audioPropertiesInfo?.linearVolume}`
          ).join(', ')
          rlog('info', `remote audio levels: ${levels}`)
        }
      })

      // Auto-subscribe + play remote audio when bot publishes
      this.engine.on(VERTC.events.onUserPublishStream, async (e: any) => {
        rlog('info', `onUserPublishStream userId=${e.userId} mediaType=${e.mediaType}`)
        if (e.mediaType === MediaType.AUDIO || e.mediaType === MediaType.AUDIO_AND_VIDEO) {
          await this.engine.subscribeStream(e.userId, MediaType.AUDIO)
          rlog('info', `subscribed to ${e.userId}`)
          try {
            await this.engine.play(e.userId)
            rlog('info', `engine.play(${e.userId}) OK`)
          } catch (err: any) {
            rlog('warn', `engine.play failed: ${err?.message}`)
          }
        }
      })

      // 5. Request mic permission via VERTC (same as official demo)
      const perms = await VERTC.enableDevices({ video: false, audio: true })
      rlog('info', `device permissions: audio=${perms.audio}`)

      // 6. Get mic device list via VERTC
      const audioInputs: MediaDeviceInfo[] = await VERTC.enumerateAudioCaptureDevices()
      rlog('info', `VERTC audio inputs (${audioInputs.length}): ${audioInputs.map(d => `[${d.deviceId.slice(0,8)}] ${d.label}`).join(' | ')}`)

      // Pick best mic: skip Voicemeeter, prefer real headset
      const SKIP = ['voicemeeter', 'vb-audio', 'steam streaming']
      const isReal = (d: MediaDeviceInfo) => !SKIP.some(s => d.label.toLowerCase().includes(s))
      const PREFER = ['hands-free ag audio', 'cx plus', 'headset', 'microphone (']
      const preferredMic = audioInputs.find(d =>
        d.deviceId !== 'default' && d.deviceId !== 'communications' &&
        isReal(d) && PREFER.some(k => d.label.toLowerCase().includes(k))
      )
      const realMic = audioInputs.find(d =>
        d.deviceId !== 'default' && d.deviceId !== 'communications' && isReal(d)
      )
      const pickedMic = preferredMic || realMic || audioInputs[0]
      if (pickedMic) {
        rlog('info', `selected mic: [${pickedMic.deviceId.slice(0,8)}] ${pickedMic.label}`)
      }

      // 7. Join room with call_scene extraInfo (required by AIGC server)
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

      // 8. Start mic capture with selected device
      await this.engine.startAudioCapture(pickedMic?.deviceId)
      rlog('info', 'mic capture started')

      // 9. Publish audio stream
      await this.engine.publishStream(MediaType.AUDIO)
      rlog('info', 'publishStream(AUDIO) OK')

      // 10. Call Volcano StartVoiceChat API via Rust
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

  // Called when user releases mic button — VAD will handle turn detection
  async endUserSpeechTurn(): Promise<void> {
    // With continuous ASR, we just log; server-side VAD detects end-of-speech
    rlog('info', 'endUserSpeechTurn — VAD handles turn detection')
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
      rlog('warn', `stop API error: ${e?.message}`)
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
        rlog('warn', `cleanup error: ${e?.message}`)
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
