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

function rlog(level: 'info' | 'warn' | 'error', msg: string) {
  console[level]('[VoiceRTC]', msg)
  invoke('js_log', { level, msg: `[VoiceRTC] ${msg}` }).catch(() => {})
}

// ── TLV codec (same as official demo) ──────────────────────────────────────
function string2tlv(str: string, type: string): ArrayBuffer {
  const typeBuffer = new Uint8Array(4)
  for (let i = 0; i < type.length; i++) typeBuffer[i] = type.charCodeAt(i)
  const valueBuffer = new TextEncoder().encode(str)
  const tlv = new Uint8Array(8 + valueBuffer.length)
  tlv.set(typeBuffer, 0)
  const len = valueBuffer.length
  tlv[4] = (len >> 24) & 0xff; tlv[5] = (len >> 16) & 0xff
  tlv[6] = (len >> 8) & 0xff;  tlv[7] = len & 0xff
  tlv.set(valueBuffer, 8)
  return tlv.buffer
}

function tlv2String(buf: ArrayBufferLike): { type: string; value: string } {
  const typeBuffer = new Uint8Array(buf, 0, 4)
  const lenBuffer  = new Uint8Array(buf, 4, 4)
  const valueBuffer = new Uint8Array(buf, 8)
  let type = ''
  for (let i = 0; i < 4; i++) type += String.fromCharCode(typeBuffer[i])
  const length = (lenBuffer[0] << 24) | (lenBuffer[1] << 16) | (lenBuffer[2] << 8) | lenBuffer[3]
  const value = new TextDecoder().decode(valueBuffer.subarray(0, length))
  return { type, value }
}

export class VoiceRTCClient {
  private config: RTCVoiceConfig
  private engine: any = null
  private roomId: string
  private userId: string
  private botUserId: string = ''
  private active: boolean = false
  private screenSharing: boolean = false

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

      // 1. Generate RTC token
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
      this.engine = VERTC.createEngine(this.config.rtcAppId)
      rlog('info', `createEngine appId=${this.config.rtcAppId}`)

      // 3. AI noise reduction (optional)
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
      // Screen share ended by system (e.g. user clicked "停止共享")
      this.engine.on(VERTC.events.onTrackEnded, async (e: any) => {
        if (e.isScreen && e.kind === 'video') {
          rlog('info', 'screen share ended by system')
          this.screenSharing = false
          try {
            await this.engine.stopScreenCapture()
            await this.engine.unpublishScreen(MediaType.VIDEO)
          } catch (_) {}
        }
      })

      // Audio level monitoring
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

      // Auto-subscribe + play remote audio
      this.engine.on(VERTC.events.onUserPublishStream, async (e: any) => {
        rlog('info', `onUserPublishStream userId=${e.userId} mediaType=${e.mediaType}`)
        if (e.mediaType === MediaType.AUDIO || e.mediaType === MediaType.AUDIO_AND_VIDEO) {
          this.botUserId = e.userId
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

      // ── Function Calling via RTC binary message (fallback when no video stream) ─
      this.engine.on(VERTC.events.onRoomBinaryMessageReceived, async (e: { userId: string; message: ArrayBuffer }) => {
        try {
          const { type, value } = tlv2String(e.message)
          if (type.trim() === 'tool') {
            const parsed = JSON.parse(value)
            const toolCalls: any[] = parsed?.tool_calls ?? []
            rlog('info', `function call: ${JSON.stringify(toolCalls)}`)
            for (const call of toolCalls) {
              const name: string = call?.function?.name ?? ''
              const callId: string = call?.id ?? call?.tool_call_id ?? ''
              if (name === 'take_screenshot') {
                rlog('info', `executing take_screenshot (tool_call_id=${callId})`)
                await this._handleScreenshot(callId, e.userId)
              }
            }
          }
        } catch (err: any) {
          rlog('warn', `binary msg handler error: ${err?.message}`)
        }
      })

      // 5. Request permissions (audio only; screen share is user-triggered)
      const perms = await VERTC.enableDevices({ video: false, audio: true })
      rlog('info', `device permissions: audio=${perms.audio}`)

      // 6. Pick best mic (skip Voicemeeter)
      const audioInputs: MediaDeviceInfo[] = await VERTC.enumerateAudioCaptureDevices()
      rlog('info', `VERTC audio inputs (${audioInputs.length}): ${audioInputs.map(d => `[${d.deviceId.slice(0,8)}] ${d.label}`).join(' | ')}`)
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
      if (pickedMic) rlog('info', `selected mic: [${pickedMic.deviceId.slice(0,8)}] ${pickedMic.label}`)

      // 7. Join room
      rlog('info', `joinRoom roomId=${this.roomId}`)
      await this.engine.joinRoom(
        token, this.roomId,
        {
          userId: this.userId,
          extraInfo: JSON.stringify({ call_scene: 'RTC-AIGC', user_name: this.userId, user_id: this.userId }),
        },
        { isAutoPublish: true, isAutoSubscribeAudio: true, roomProfileType: RoomProfileType.chat }
      )
      rlog('info', 'joined room OK')

      // 8. Start mic
      await this.engine.startAudioCapture(pickedMic?.deviceId)
      rlog('info', 'mic capture started')
      await this.engine.publishStream(MediaType.AUDIO)
      rlog('info', 'publishStream(AUDIO) OK')

      // 9. Start screen sharing (user will see browser prompt)
      let videoEnabled = false
      try {
        await this.engine.startScreenCapture({ enableAudio: false })
        await this.engine.publishScreen(MediaType.VIDEO)
        this.screenSharing = true
        videoEnabled = true
        rlog('info', 'screen capture started and published ✅')
      } catch (e: any) {
        rlog('warn', `screen capture failed (will use function call fallback): ${e?.message}`)
      }

      // 10. StartVoiceChat API
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
        enableVideo: videoEnabled,
      })
      rlog('info', `StartVoiceChat OK — voice chat active! (video=${videoEnabled})`)

      this.active = true
    } catch (error: any) {
      rlog('error', `start failed: ${error?.message ?? String(error)}`)
      await this._cleanup()
      throw error
    }
  }

  // ── Screenshot function call handler ────────────────────────────────────
  private async _handleScreenshot(toolCallId: string, botUserId: string): Promise<void> {
    try {
      // 1. Take screenshot
      const imageBase64 = await invoke<string>('take_screenshot')
      rlog('info', 'screenshot taken')

      // 2. Analyze with vision LLM
      const description = await invoke<string>('chat_with_pet', {
        screenshot: imageBase64,
        message: '请简洁描述屏幕上的主要内容，用户在做什么',
        llmEndpointId: this.config.llmEndpointId,
        llmApiKey: this.config.llmApiKey,
        characterPrompt: '你是屏幕内容分析助手，简洁描述屏幕上的主要内容',
      })
      rlog('info', `screenshot analyzed: ${description.slice(0, 100)}`)

      // 3. Send result back via RTC binary message
      const resultPayload = JSON.stringify({
        ToolCallID: toolCallId,
        Content: description,
      })
      this.engine.sendUserBinaryMessage(botUserId, string2tlv(resultPayload, 'tool'))
      rlog('info', `screenshot result sent to ${botUserId}`)

      // 4. Also send via UpdateVoiceChat API as backup
      await invoke('update_rtc_voice_chat', {
        appId: this.config.rtcAppId,
        accessKeyId: this.config.accessKeyId,
        secretAccessKey: this.config.secretAccessKey,
        roomId: this.roomId,
        command: 'function',
        message: resultPayload,
      }).catch((e: any) => rlog('warn', `UpdateVoiceChat function result failed: ${e?.message}`))
    } catch (err: any) {
      rlog('error', `screenshot handler failed: ${err?.message}`)
      const errPayload = JSON.stringify({ ToolCallID: toolCallId, Content: '截图失败，无法获取屏幕内容' })
      try { this.engine.sendUserBinaryMessage(botUserId, string2tlv(errPayload, 'tool')) } catch (_) {}
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
      rlog('warn', `stop API error: ${e?.message}`)
    }
    await this._cleanup()
  }

  async endUserSpeechTurn(): Promise<void> {
    rlog('info', 'endUserSpeechTurn — VAD handles turn detection')
  }

  private async _cleanup() {
    if (this.engine) {
      try {
        await this.engine.stopAudioCapture()
        if (this.screenSharing) {
          await this.engine.stopScreenCapture()
          await this.engine.unpublishScreen(MediaType.VIDEO)
          this.screenSharing = false
        }
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

  isActive(): boolean { return this.active }
}
