# RTC Token & VoiceChat API Implementation

## Task
Implement the two placeholder Rust functions in frontend/src-tauri/src/lib.rs:
1. `generate_rtc_token` - Generate a real Volcano Engine RTC token using HMAC-SHA256
2. `start_rtc_voice_chat` - Call Volcano Engine StartVoiceChat API with proper signing
3. `stop_rtc_voice_chat` - Call Volcano Engine StopVoiceChat API

## 1. generate_rtc_token implementation

Volcano Engine RTC token generation algorithm:
- Reference: https://www.volcengine.com/docs/6348/70121
- The token is a joined string of fields separated by ':'

Add hmac crate to Cargo.toml:
```toml
hmac = "0.12"
sha2 = "0.10"
```

Implementation:
```rust
#[tauri::command]
async fn generate_rtc_token(
    app_id: String,
    app_key: String,
    room_id: String,
    user_id: String,
    access_key_id: String,
    secret_access_key: String,
) -> Result<String, String> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    use base64::{Engine as _, engine::general_purpose};

    // Read env overrides
    let key = std::env::var("VOLCANO_RTC_APP_KEY").unwrap_or(app_key);

    // Token fields
    let version = "001";
    let app_id_str = &app_id;
    let room_id_str = &room_id;
    let user_id_str = &user_id;
    
    // expire_at: current timestamp + 24 hours
    let expire_at = unix_now() + 86400;
    
    // nonce: random 32-bit number as hex
    let nonce = format!("{:08x}", rand_u32());
    
    // privilege: "Pub=1" means can publish stream
    let privilege = "Pub=1";
    
    // Build the string to sign
    let sign_content = format!("{}\n{}\n{}\n{}\n{}\n{}\n{}",
        version, app_id_str, room_id_str, user_id_str, expire_at, nonce, privilege
    );
    
    // HMAC-SHA256
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(key.as_bytes())
        .map_err(|e| format!("HMAC key error: {e}"))?;
    mac.update(sign_content.as_bytes());
    let signature = general_purpose::STANDARD.encode(mac.finalize().into_bytes());
    
    // Final token format: version:appId:roomId:userId:expireAt:nonce:privilege:signature
    let token = format!("{}:{}:{}:{}:{}:{}:{}:{}",
        version, app_id_str, room_id_str, user_id_str, expire_at, nonce, privilege, signature
    );
    
    Ok(token)
}

fn rand_u32() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ns = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().subsec_nanos();
    ns ^ (ns << 13) ^ (ns >> 7)
}
```

## 2. start_rtc_voice_chat implementation

Call Volcano Engine RTC VisualVoiceChat Start API:
- URL: https://rtc.volcengineapi.com/?Action=StartVoiceChat&Version=2024-12-01
- Method: POST
- Auth: Volcano V4 signature (HMAC-SHA256)

The Volcano V4 signing process is complex. Use a simplified approach:
- Use reqwest to POST with Authorization header
- The signing uses AccessKeyID + SecretAccessKey

Request body structure (based on yuki's DoubaoRTCClient.js):
```json
{
  "AppId": "<RTC AppId>",
  "RoomId": "<room id>",
  "TaskId": "<task id = room id>",
  "Config": {
    "WelcomeMessage": "你好！我在这里陪你！",
    "TargetUserId": ["<user id>"],
    "BotConfig": {
      "BotName": "小宠物",
      "BotUserId": "bot_assistant",
      "Language": "zh-CN",
      "ASRConfig": {
        "ProviderType": "volcano",
        "ProviderParams": {
          "Mode": "bigmodel",
          "AppId": "<ASR AppId>",
          "AccessToken": "<ASR AccessToken>",
          "ApiResourceId": "volc.seedasr.sauc.duration",
          "StreamMode": 2,
          "VolcanoASRParameters": "{\"request\":{\"enable_nonstream\":true}}"
        }
      },
      "TTSConfig": {
        "ProviderType": "volcano",
        "ProviderParams": {
          "app": {
            "appid": "<TTS AppId>",
            "token": "<TTS AccessToken>"
          },
          "audio": {
            "voice_type": "ICL_zh_female_keainvsheng_tob",
            "speech_rate": 0
          },
          "ResourceId": "seed-tts-1.0"
        }
      },
      "LLMConfig": {
        "ProviderType": "volcano",
        "Mode": "ArkV3",
        "EndPointId": "<LLM EndpointId>",
        "ApiKey": "<LLM ApiKey>",
        "SystemPrompt": "<character prompt>",
        "MaxTokens": 1024,
        "Temperature": 0.7,
        "TopP": 0.9
      }
    }
  }
}
```

For the Volcano API signature, implement a helper function `volcano_sign_request` that:
1. Creates the canonical request string
2. Signs with HMAC-SHA256 using SecretAccessKey
3. Adds Authorization header

Reference signing format:
```
Authorization: HMAC-SHA256 Credential=<AccessKeyId>/date/region/service/request, SignedHeaders=content-type;host;x-date, Signature=<hex_signature>
```

The signing steps:
1. x-date header = current UTC time in format: 20060102T150405Z (Go time format, i.e. YYYYMMDDTHHmmssZ)
2. credential_scope = date + "/" + "cn-north-1" + "/" + "rtc" + "/" + "request"
3. canonical_headers = "content-type:application/json\nhost:rtc.volcengineapi.com\nx-date:" + x_date + "\n"
4. signed_headers = "content-type;host;x-date"
5. hashed_payload = hex(sha256(body))
6. canonical_request = "POST\n/\nAction=StartVoiceChat&Version=2024-12-01\n" + canonical_headers + "\n" + signed_headers + "\n" + hashed_payload
7. string_to_sign = "HMAC-SHA256\n" + x_date + "\n" + credential_scope + "\n" + hex(sha256(canonical_request))
8. signing_key = hmac_sha256(hmac_sha256(hmac_sha256(hmac_sha256(secret_access_key, date), "cn-north-1"), "rtc"), "request")
9. signature = hex(hmac_sha256(signing_key, string_to_sign))
10. Authorization = format!("HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}", access_key_id, credential_scope, signed_headers, signature)

## 3. stop_rtc_voice_chat implementation

Same signing approach, but:
- URL: https://rtc.volcengineapi.com/?Action=StopVoiceChat&Version=2024-12-01  
- Body: `{"AppId": "<appId>", "RoomId": "<roomId>", "TaskId": "<roomId>"}`

## Cargo.toml additions needed:
```toml
hmac = "0.12"
sha2 = "0.10"
hex = "0.4"
```
Check if sha2 already exists before adding.

## Important notes:
- Read env vars for all sensitive values (pattern already established in codebase)
- After implementing, run `cargo check` to verify compilation
- Log important steps with log::info!
- Handle errors gracefully with descriptive messages

## After implementation:
1. Run `cargo check` 
2. Run `cd frontend && npx tsc --noEmit`
3. Run `cd frontend && git add -A && git commit -m "feat: implement RTC token generation and VoiceChat API"`
4. Report which files changed and if all checks passed
