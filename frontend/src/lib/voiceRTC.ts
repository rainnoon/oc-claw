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
    this.roomId = `oc_${Date.now()}`
    this.userId = `u_${Date.now()}`
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

      // 2. Create engine
      rlog('info', `createEngine appId=${this.config.rtcAppId}`)
      this.engine = VERTC.createEngine(this.config.rtcAppId)

      // 3. Listen for bot joining (AI ready)
      this.engine.on(VERTC.events.onUserJoined, (e: any) => {
        rlog('info', `user joined: ${e.userInfo?.userId}`)
      })

      // 4. Auto-subscribe remote audio (AI voice)
      this.engine.on(VERTC.events.onUserPublishStream, async (e: any) => {
        rlog('info', `onUserPublishStream userId=${e.userId} mediaType=${e.mediaType}`)
        if (e.mediaType === MediaType.AUDIO) {
          await this.engine.subscribeStream(e.userId, MediaType.AUDIO)
          rlog('info', `subscribed to audio from ${e.userId}`)
        }
      })

      // 5. Join room
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
