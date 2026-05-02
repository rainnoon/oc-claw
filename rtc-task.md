# RTC Voice Companion Implementation Task

## Goal
Upgrade oc-claw's voice companion to use Volcano Engine RTC real-time voice (like yuki game).

## Reference: yuki's core config structure
```js
// RTC
appId = config.appId
appKey = config.appKey

// ASR Config
ASRConfig: {
  ProviderParams: {
    Mode: 'bigmodel',
    AppId: 'ASR_AppId',
    AccessToken: 'ASR_AccessToken',
    ApiResourceId: 'volc.seedasr.sauc.duration',
    StreamMode: 2,
    VolcanoASRParameters: '{"request":{"enable_nonstream":true}}'
  }
}

// LLM Config
LLMConfig: {
  EndPointId: 'LLM_EndpointId',
  ApiKey: 'LLM_ApiKey'
}

// TTS Config
TTSConfig: {
  ProviderParams: {
    app: { appid: 'TTS_AppId', token: 'TTS_AccessToken' },
    audio: { voice_type: 'ICL_zh_female_keainvsheng_tob', speech_rate: 0 },
    ResourceId: 'seed-tts-1.0'
  }
}
```

## Steps

### Step 1: Install npm dependencies
```bash
cd frontend
npm install @volcengine/rtc
```

### Step 2: Update frontend/src/lib/store.ts
Add `accessKeyId` and `secretAccessKey` to VoiceConfig interface and loadVoiceConfig defaults.

### Step 3: Create frontend/src/lib/voiceRTC.ts
Create VoiceRTCClient class:
- constructor(config: RTCVoiceConfig)
- startVoiceChat(): Promise<void> - creates VERTC engine, joins room, starts mic, calls start_rtc_voice_chat invoke
- stopVoiceChat(): Promise<void> - calls stop_rtc_voice_chat invoke, leaves room, destroys engine
- isActive(): boolean

Use @volcengine/rtc for the RTC engine. Call Tauri invoke for token generation and API calls.

RTCVoiceConfig interface:
```typescript
interface RTCVoiceConfig {
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
```

### Step 4: Add Rust commands to frontend/src-tauri/src/lib.rs

Add these 3 commands (placeholder implementations with TODO comments):

```rust
#[tauri::command]
async fn generate_rtc_token(
    app_id: String, app_key: String, room_id: String, user_id: String,
    access_key_id: String, secret_access_key: String,
) -> Result<String, String> {
    // TODO: implement HMAC-SHA256 RTC token generation
    // Read env overrides first
    let key = std::env::var("VOLCANO_RTC_APP_KEY").unwrap_or(app_key);
    Ok(format!("placeholder_token_{}_{}", app_id, room_id))
}

#[tauri::command]
async fn start_rtc_voice_chat(
    app_id: String, access_key_id: String, secret_access_key: String,
    room_id: String, user_id: String,
    asr_app_id: String, asr_access_token: String,
    tts_app_id: String, tts_access_token: String,
    llm_endpoint_id: String, llm_api_key: String, character_prompt: String,
) -> Result<(), String> {
    // TODO: call Volcano VisualVoiceChat Start API with HMAC-SHA256 signing
    // POST https://rtc.volcengineapi.com/?Action=StartVoiceChat&Version=2024-12-01
    let ak = std::env::var("VOLCANO_ACCESS_KEY_ID").unwrap_or(access_key_id);
    let sk = std::env::var("VOLCANO_SECRET_ACCESS_KEY").unwrap_or(secret_access_key);
    log::info!("[RTC] start_rtc_voice_chat room={} user={}", room_id, user_id);
    Ok(())
}

#[tauri::command]
async fn stop_rtc_voice_chat(
    app_id: String, access_key_id: String, secret_access_key: String, room_id: String,
) -> Result<(), String> {
    // TODO: call Volcano StopVoiceChat API
    log::info!("[RTC] stop_rtc_voice_chat room={}", room_id);
    Ok(())
}
```

Register all 3 in invoke_handler in run().

### Step 5: Update frontend/src/Mini.tsx

Change the voice button to press-and-hold behavior:
- onMouseDown -> handleVoiceStart
- onMouseUp + onMouseLeave -> handleVoiceStop
- onTouchStart -> handleVoiceStart
- onTouchEnd -> handleVoiceStop

Button turns red when isVoiceActive=true.

handleVoiceStart:
1. Load voice config
2. If accessKeyId or rtcAppId empty -> fall back to existing handleVoiceTalk() (HTTP mode)
3. Else: create VoiceRTCClient, store in voiceClientRef, call startVoiceChat(), setIsVoiceActive(true)

handleVoiceStop:
1. If voiceClientRef.current exists, call stopVoiceChat()
2. setIsVoiceActive(false), clear ref

### Step 6: Update frontend/src/components/SettingsTab.tsx
Add accessKeyId and secretAccessKey fields (password type) to the voice config section.

### Step 7: Update frontend/src-tauri/.env.local
Add:
```
VOLCANO_ACCESS_KEY_ID=
VOLCANO_SECRET_ACCESS_KEY=
VOLCANO_RTC_APP_ID=
VOLCANO_RTC_APP_KEY=
VOLCANO_ASR_APP_ID=
VOLCANO_ASR_ACCESS_TOKEN=
```

## Validation
1. Run: cd frontend && npm install @volcengine/rtc
2. Run: cd frontend/src-tauri && cargo check
3. Run: cd frontend && npx tsc --noEmit
4. All must pass with no errors

## Important notes
- Keep existing HTTP fallback working (handleVoiceTalk)
- Don't break any existing functionality
- Style should match existing code
- VoiceRTCClient should handle errors gracefully

When done, summarize which files were changed and whether all checks pass.
