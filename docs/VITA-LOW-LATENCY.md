# PS Vita xCloud: lowest practical latency without unstable decoding

Baseline: November 22, 2026. These changes are experimental and were designed for the PS Vita's original 960x544 display, hardware H.264 decoder, and legacy 2.4 GHz Wi-Fi.

## Key distinction: local PS Remote Play versus remote cloud

A PS4 Remote Play session on the same LAN, or an appropriate local streaming server, generally avoids the external Internet round trip to Microsoft's Xbox Cloud Gaming servers. PS Vita official Remote Play did have latency; a sufficiently good local setup can make it hard to notice. GreenVita cannot remove round-trip server and game-engine delay. End-to-end button-to-photon latency requires external measurement (e.g. a high-speed camera with controller LED).

## What was already implemented

- 1 ms RTC pump and independently running H.264 hardware decoder with zero-copy RGB565 output to 960x544 double-buffered SDL/GXM textures.
- 4 ms control sampling within the 16 ms rendering frame budget; changed gamepad states supersede stale unprocessed states.
- Packet loss recovery via bounded NACK, 32 ms / 48-packet reordering grace, and bounded REMB adaptive bitrate requests.
- 2–6 compressed access unit decoder queue depending on detected framerate, latest-frame replacement at presentation, codec resynchronization and PLI requests.
- Original controller mapping, front/rear touch controls, and experimental console-to-web fallback.

## Added in low-latency branch

1. **RTC receive fairness:** `RtcTransport::receive` handles at most **96 UDP datagrams** in one worker pump. This avoids pathological continuous receive loops starving fresh gamepad sends. The socket remains readable, so the next loop can drain the next batch.
2. **Fresh controls after render:** refresh SDL controller state and send changed input immediately after the scene has been presented, *also* retaining the existing 4 ms polling while waiting for the next frame. The UI does not increase its 60 Hz target refresh.
3. **Bounded audio latency:** adjust playback startup from 80 to **60 ms** and audio output backlog cap from 240 to **160 ms**. This prevents sound from trailing a live picture excessively; sparse/unstable Wi-Fi may produce more dropouts, so validate on a physical PS Vita.
4. **Input queue telemetry:** `inputq:` measures microseconds from local controller sampling to actual successful dispatch through the RTC data-channel method; this is **not** the same as end-to-end controller-to-screen latency. Enable Settings -> stream debug view to inspect it.
5. **Existing stream metrics:** `rx:` is received video RTP payload Mbps, `remb:` is an unconfirmed requested bitrate, `nack:` counts successfully submitted RTCP NACK requests and sequence references.

## Tests / rollback conditions

Test both stable 720p30 and 720p60 titles, if a title/server offers them. This code **does not negotiate an artificial FPS mode** or force 60fps on a server.
- With normal 2.4 GHz Wi-Fi, run a 10-minute gameplay test with full analog movement and button mashing. Watch `inputq:`, `q:`, `skip:`, `repl:`, `rs:`, `drop:`, `err:`, `rx:`, `remb:`, and stability. Compare 30fps games to 60fps games.
- Re-test with weaker Wi-Fi, Bluetooth in use, heavy screen movement, and after a short network interruption. Look for increased audio underruns, more keyframe requests and complete application hangs.
- If sound cracks or frequently pauses, roll back the 60/160 ms audio change before adjusting UDP fairness or gamepad handling.
- If frames freeze while audio continues, retain the original 32 ms NACK reorder grace; do not blindly shrink it to 0–5ms or drop H.264 predictive frames out of order.
- Compare input responsiveness against an unchanged release on the same game, account, server region, Wi-Fi and PS Vita, ideally with a high-speed camera.

## Further optimizations requiring hardware profiling

- Optional balanced/low-latency video reorder policies keyed to real packet jitter, with repeated loss testing. Smaller jitter buffers may reduce delay but can lead to much more corrupted video.
- Frame queue age histogram from RTP arrival to actual screen flip; distinguish this from decoder-only time.
- Smart audio headroom (e.g. 50–100 ms on other platforms) with recovery without hard queue clears.
- Validate Vita's SDL2/GXM presentation path before experimenting with VSync bypass; an unsynchronized display flip can cause tearing or unstable frame pacing.
- Optional conservative bitrate cap after measuring loss (test 2.5–4 Mbps) instead of requesting high 8Mbps under congested Wi-Fi.
- Optional LAN-friendly Xbox Remote Play when available; does not rely on cloud-console entitlement trickery.

## Practical network checklist

- PS Vita retail Wi-Fi is normally 2.4 GHz (802.11b/g/n, 1x1), **not** 5 GHz. Use a clean 2.4 GHz channel, short distance from the access point, good reception and minimum Bluetooth competition.
- Prefer connecting the home router to the Internet modem and any local Xbox via Ethernet. Keep heavy downloads and high-bandwidth uploads off the network during testing.
- Avoid VPNs and proxies unless a specific measured route genuinely improves network quality; they can add RTT.
- Console-local Remote Play and cloud Xbox gameplay have different minimum achievable latency.
- No code modification can make a faraway or congested xCloud region operate with zero round-trip time.

## Sources
- Sony PS Vita PCH-2001 manual (Wi-Fi 802.11b/g/n, 1x1): https://manuals.playstation.net/document/pdf/PCH-2001-3.5_3.pdf
- Moonlight latency FAQ: https://github.com/moonlight-stream/moonlight-docs/wiki/Frequently-Asked-Questions
- Vita Moonlight Relay performance review: https://github.com/dubleyu/vita-moonlight-relay/blob/main/PERFORMANCE_OVERHAUL.md
- VitaRPS5 latency changes: https://www.gamebrew.org/VitaRPS5
- XCloud4 release notes / low-latency presentation and adaptive audio: https://github.com/asp11edgr/XCloud4/releases
- OpenNOW Vita streaming profile/CPU pinning: https://github.com/OpenCloudGaming/OpenNOW-vita/releases

Implementation and tests on separate development branch until hardware acceptance criteria pass; no change to `master`.
