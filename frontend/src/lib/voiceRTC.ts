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
      // 1. Generate RTC token via Rust
      const token = await invoke<string>('generate_rtc_token', {
        appId: this.config.rtcAppId,
        appKey: this.config.rtcAppKey,
        roomId: this.roomId,
        userId: this.userId,
        accessKeyId: this.config.accessKeyId,
        secretAccessKey: this.config.secretAccessKey,
      })
      console.log('[VoiceRTC] token generated')

      // 2. Create engine
      this.engine = VERTC.createEngine(this.config.rtcAppId)

      // 3. Listen for bot joining (AI ready)
      this.engine.on(VERTC.events.onUserJoined, (e: any) => {
        console.log('[VoiceRTC] user joined:', e.userInfo?.userId)
      })

      // 4. Auto-subscribe remote audio (AI voice)
      this.engine.on(VERTC.events.onUserPublishStream, async (e: any) => {
        if (e.mediaType === MediaType.AUDIO) {
          await this.engine.subscribeStream(e.userId, MediaType.AUDIO)
        }
      })

      // 5. Join room — Volcano RTC API: joinRoom(token, roomId, userInfo, options)
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
      console.log('[VoiceRTC] joined room:', this.roomId)

      // 6. Start mic
      await this.engine.startAudioCapture()
      console.log('[VoiceRTC] mic started')

      // 7. Call Volcano StartVoiceChat API via Rust
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
      console.log('[VoiceRTC] StartVoiceChat called')

      this.active = true
    } catch (error) {
      console.error('[VoiceRTC] start failed:', error)
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
    } catch (e) {
      console.warn('[VoiceRTC] stop API error:', e)
    }

    await this._cleanup()
  }

  private async _cleanup() {
    if (this.engine) {
      try {
        await this.engine.stopAudioCapture()
        await this.engine.leaveRoom()
        VERTC.destroyEngine(this.engine)
      } catch (e) {
        console.warn('[VoiceRTC] cleanup error:', e)
      }
      this.engine = null
    }
    this.active = false
    console.log('[VoiceRTC] cleaned up')
  }

  isActive(): boolean {
    return this.active
  }
}
