# GTA VI on GreenVita: evidence, compatibility, and PS Vita limits

Research baseline: 2026-11-22. This is an **experimental compatibility project**, not a confirmed GTA VI client.

## What has actually been demonstrated?

- Official Xbox Wire (2026-11-10) says GTA VI launched on Xbox Cloud Gaming for Xbox One and Xbox Series X|S on 2026-11-19. The game runs in Microsoft's cloud, not on the local console.
- Streaming from the Xbox One has been reported and demonstrated. These reports **do not** establish PlayStation Vita/PS4/PS5 client authorization.
- An article about XCloud4 1.3.0-beta claims that GTA VI works on a jailbroken PS4. The release notes for that beta (published before GTA VI launched) describe general streaming, automatic reconnection, TLS, audio recovery, and UI improvements; they do not document a successful console-only game session or a verified bypass.
- PSBox developer RafaelNGP documented that console-only titles are gated by device allowlists, and subsequently reported unsuccessful spoofing attempts.
- Public GBAtemp research reported >250 metadata and User-Agent variations without success on a restricted test title (`DeviceTypeNotAllowed`). This is a report by an independent researcher, not a proof of what Microsoft uses internally.

**Important:** Identifying a cloud title in a catalogue, obtaining an `xgpuconsole` credential, creating a cloud session, and decoding the first frame are four different outcomes. Only a decoded frame under that offering establishes usable video. None establishes that an unauthenticated device is an Xbox.

Sources:
- https://news.xbox.com/pt-br/2026/11/10/grand-theft-auto-vi-chega-ao-xbox-one-e-xbox-series-xs-por-meio-do-xbox-cloud-gaming/
- https://github.com/RafaelNGP/psbox-cloud-gaming/issues/480
- https://github.com/asp11edgr/XCloud4/releases/tag/v1.3.0-beta
- https://github.com/asp11edgr/XCloud4/issues/15
- https://gbatemp.net/threads/research-xbox-cloud-gaming-console-authentication-on-pc-xsts-devicetypenotallowed-emulation.686691/

## PS Vita realities

- Sony display resolution is 960×544; memory is 512 MB main / 128 MB VRAM, with less available to an individual app.
- Keep the **existing hardware H.264 decode path**, cap incoming frames to 1280×720, preserve the original UI and one-controller input.
- Do not transplant PS4/PS5 GPU-upscaling, 1080p/4K decoding, Vulkan, AMD FSR or 4-controller profiles into the Vita pipeline.
- NACK feedback helps only if the sender retains missing packets. REMB expresses a requested bitrate and is not proof that the server accepted it.
- Encrypted Xbox hardware identity or extracting another console's device keys is not implemented.

## Implemented on branch `vita-recovery-and-stream-telemetry`

1. **Bounded reconnection after playback**: after a backend `Closed` event, retry once per manually launched title, *only if a decoded video frame was previously received*. Reuse the ordinary start-stream path and its existing console-to-web fallback. Never loop indefinitely or treat an entitlement rejection as a network drop.
2. **Received stream telemetry**: the once-per-second stream status includes approximate video RTP payload throughput (`rx:` Mbps) independently of the requested REMB (`remb:` Mbps). This is not full wire-throughput or a measurement of server encoder bitrate.
3. **NACK telemetry**: status shows cumulative successfully submitted RTCP NACK requests and missing packet references (`nack:requests/packets`). It does not prove the Xbox server retransmitted the packets.
4. **Vita CI on this branch**: the normal container-based VitaSDK workflow builds the VPK on branch push, without changing the main or existing diagnostic branch.

This work is independent, written for the original GreenVita Rust/PS Vita pipeline, and does not copy GPL-only PS4 application source into GreenVita.

## Manual Vita acceptance checklist

1. Build the VPK and verify GitHub Actions reports **success**. This is necessary but insufficient.
2. Install on a real Vita, sign in using the original Microsoft device flow, and start a **known working** cloud title.
3. Check that actual video/audio and Vita controls function. Confirm the displayed `enc:` size is at most 1280×720, and `rx:` and `remb:` are distinct values.
4. While a known working title is playing, briefly interrupt network connectivity and restore it. A backend `Closed` event should initiate at most one automatic rejoin; other failures may still require a manual retry.
5. Inspect `ux0:data/xcloud-rust/connection-status.txt` for `network_rejoin: ONE_RETRY_AFTER_DECODED_VIDEO`. It is a *client attempt*, not a guarantee of reconnection.
6. Test a genuinely entitled restricted title separately. Record only allowlisted diagnostic status: `DeviceTypeNotAllowed`, console session accepted, fallback, and `CONSOLE_FIRST_DECODED_FRAME` or `WEB_FIRST_DECODED_FRAME`. Do not interpret an error-free login as playable GTA VI.
7. Never publish Microsoft credentials, refresh tokens, raw HTTP responses, console secrets, account/session IDs, or unredacted logs.

No live Xbox login, account-specific entitlement tests, GTA VI session, or physical PS Vita hardware validation was performed by the automated source changes.
