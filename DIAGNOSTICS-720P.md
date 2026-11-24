# GreenVita: Xbox connection diagnostics and Vita-safe 720p reception

This experimental update keeps **the original GreenVita UI and controls**.
No 1080p/1440p modes are added. H.264 decode is capped at **1280×720**, with
the same 960×544 GXM output; oversized SPS access units are rejected and
a compatible IDR is requested.

## Private, on-device diagnostic file

Open **VitaShell → ux0:data/xcloud-rust/connection-status.txt** after signing in,
opening the game library, and trying to start a game. The file is limited
to 32 recent status events, and records only stage labels and allowlisted
server refusal types. **No Microsoft passwords, tokens, console secrets,
authorization headers, account IDs or session IDs are written to it.**

| Stage | What it establishes | What it does NOT establish |
| --- | --- | --- |
| `xgpuconsole_token: ACCEPTED_NOT_HARDWARE_PROOF` | Console offering login returned credentials | Xbox hardware identity or game authorization |
| `xgpuconsole_session: CREATED_NOT_VIDEO_PROOF` | Console offering created a session | Functional WebRTC or frame decode |
| `console_state: PROVISIONED_NOT_VIDEO_PROOF` | Console session was provisioned | Decoded game video |
| `webrtc_video: CONSOLE_FIRST_DECODED_FRAME` | A real H.264 frame arrived and decoded under the console offering | Hardware attestation or availability of other titles |
| `fallback: XGPUWEB_REQUESTED` | The original web offering is being attempted | That the web session succeeded |
| `webrtc_video: WEB_FIRST_DECODED_FRAME` | The original web offering produced decoded video | That console spoofing worked |
| `session_state: DeviceTypeNotAllowed` | The server refused the selected device type | Whether alternative authorized clients could run the title |

The game library now also parses `details.allowedDeviceTypes`. The file
records the advertised classes for the **selected game**, if the xCloud
web catalog provides them. If a title is absent, no entitlement, price
or cloud compatibility is fabricated. Presence in the catalog does not
prove the current offering is allowed to launch it.

For a legitimate owned/entitled console-restricted title (for example
**Killer Instinct 2 Classic**, only where it is available in your account's
catalog), compare:
1. Console offering token status
2. Console session HTTP/play result
3. Provisioning and `DeviceTypeNotAllowed`, if present
4. Web fallback result
5. First decoded frame and the offering it used

Do **not** buy a game just to test this. This build cannot bypass device
entitlements or reproduce physical Xbox cryptographic proof.

## Transport improvements

- Explicit receiver-side RTCP **NACK** requests for missing video RTP
  packets, rather than just advertising `nack` in the SDP; bounded to
  24 missing packet reports per request with an expiry of 180 ms.
- A bounded **32 ms / 48 RTP-packet** cross-frame H.264 reorder grace.
  NACK has an opportunity to recover missing packets before an IDR restart.
- Receiver **REMB** requests every 5 seconds (and after congestion changes),
  starting at 6 Mbps; packet/frame damage lowers the request by 20%, repeated
  clean windows increase it by 1 Mbps every 20 seconds, range **2.5–15 Mbps**.
  The 15 Mbps ceiling is experimental and is not a guaranteed video bitrate.
- Existing keyframe/PLI, H.264 hardware-decoder reset and 720p SPS
  limit remain in force. A decoding error/recovery writes one bounded
  diagnostic event; it does not cause an unbounded retry loop.

The server may **ignore bitrate requests** and NACK retransmissions can only
work if the sender retains the missing packets. The diagnostics file confirms
client actions, not the server's actual encoder bitrate.

## Hardware validation

Compile success is necessary but not sufficient. On an actual Vita,
test a normal cloud title first; check first video/audio, controls, time
to start, and that quality never exceeds 1280×720. Then test an entitled
restricted title and inspect the file. If a trial stalls, include only the
redacted diagnostic text in a bug report.

The PS4 projects XCloud4, Nexo X4 and PSBox are protocol references, not
drop-in media decoders for PS Vita. This build makes no claim of copying
their GPL source into GreenVita's MPL-2.0 codebase.

## Experimental v0.3.0 streaming telemetry
- `jitter:Nms`: receiver-side smoothed RTP interarrival variation, not network round-trip latency.
- `gaps:N`: missing sequence references detected, not permanent packet loss.
- `late:N`: packets seen after a gap, not proof a retransmission was used.
- `displayq:Nus`: decoded frame age through local SDL present return; may include scheduler and VSync time.
- `pipe:Nus`: compressed access-unit queue to SDL present return, not the server's video pipeline.
- `audio:Nms`, `au:N/N`: adaptive audio start target, underruns and skipped PCM due to backlog.
- Selected Quality profile defaults to 15 Mbps requested maximum; Balanced requests at most 10 Mbps, Low Latency 6 Mbps. Changes apply on next stream.
- Authorized region selection is for **future** sessions. HTTPS HEAD timing does not estimate cloud gaming RTT.
