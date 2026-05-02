import { invoke } from '@tauri-apps/api/core'
import VERTC from '@volcengine/rtc'

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
    // Generate unique IDs for this session
    this.roomId = `room_${Date.now()}`
    this.userId = `user_${Date.now()}`
  }

  async startVoiceChat(): Promise<void> {
    if (this.active) {
      console.warn('[VoiceRTC] Already active')
      return
    }

    try {
      // Generate RTC token
      const token = await invoke<string>('generate_rtc_token', {
        appId: this.config.rtcAppId,
        appKey: this.config.rtcAppKey,
        roomId: this.roomId,
        userId: this.userId,
        accessKeyId: this.config.accessKeyId,
        secretAccessKey: this.config.secretAccessKey,
      })

      // Create VERTC engine
      this.engine = VERTC.createEngine(this.config.rtcAppId)

      // Join room
      await this.engine.joinRoom(
        token,
        {
          roomId: this.roomId,
          userId: this.userId,
        },
        {
          isAutoPublish: true,
          isAutoSubscribeAudio: true,
        }
      )

      // Start microphone capture
      await this.engine.startAudioCapture()

      // Start the backend voice chat session
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
        characterPrompt: this.config.characterPrompt,
      })

      this.active = true
      console.log('[VoiceRTC] Started successfully')
    } catch (error) {
      console.error('[VoiceRTC] Failed to start:', error)
      // Clean up on error
      if (this.engine) {
        try {
          await this.engine.leaveRoom()
          this.engine.destroy()
        } catch (e) {
          console.error('[VoiceRTC] Cleanup error:', e)
        }
        this.engine = null
      }
      throw error
    }
  }

  async stopVoiceChat(): Promise<void> {
    if (!this.active) {
      return
    }

    try {
      // Stop the backend voice chat session
      await invoke('stop_rtc_voice_chat', {
        appId: this.config.rtcAppId,
        accessKeyId: this.config.accessKeyId,
        secretAccessKey: this.config.secretAccessKey,
        roomId: this.roomId,
      })
    } catch (error) {
      console.error('[VoiceRTC] Failed to stop backend session:', error)
    }

    // Leave room and destroy engine
    if (this.engine) {
      try {
        await this.engine.leaveRoom()
        this.engine.destroy()
      } catch (error) {
        console.error('[VoiceRTC] Failed to leave room:', error)
      }
      this.engine = null
    }

    this.active = false
    console.log('[VoiceRTC] Stopped')
  }

  isActive(): boolean {
    return this.active
  }
}
