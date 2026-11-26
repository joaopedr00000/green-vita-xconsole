use crate::App;
use crate::app::ui::theme::Theme;
use crate::app::ui::widgets::draw_hold_progress_ring;
use crate::i18n::I18n;

/// Fullscreen video view with a one-time "Hold Back to pause" hint, fading out over `HINT_FADE`.
pub(crate) fn show(ctx: &egui::Context, app: &App, hold_progress: Option<f32>) {
    const HINT_VISIBLE: std::time::Duration = std::time::Duration::from_secs(2);
    const HINT_FADE: std::time::Duration = std::time::Duration::from_secs(1);

    let theme = Theme::dark();
    let i18n = I18n::new(app.settings.locale);
    let streaming = match &app.state {
        crate::AppState::Streaming(streaming) => streaming,
        _ => return,
    };
    let mut frame = egui::Frame::central_panel(&ctx.style());
    frame.fill = egui::Color32::TRANSPARENT;
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        let elapsed = streaming.hint_started_at.elapsed();
        let alpha = if elapsed < HINT_VISIBLE {
            1.0
        } else if elapsed < HINT_VISIBLE + HINT_FADE {
            1.0 - (elapsed - HINT_VISIBLE).as_secs_f32() / HINT_FADE.as_secs_f32()
        } else {
            0.0
        };

        if alpha > 0.0 {
            ui.vertical_centered(|ui| {
                ui.add_space(16.0);
                ui.colored_label(
                    theme.text.gamma_multiply(alpha),
                    i18n.text("streaming-hold-back"),
                );
            });
        }


    });

    if app.settings.show_stream_debug_info { show_diagnostics(ctx,app,streaming); }

    if let Some(progress) = hold_progress {
        egui::Area::new(egui::Id::new("pause_hold_indicator"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::LEFT_TOP, egui::vec2(16.0, 16.0))
            .show(ctx, |ui| {
                draw_hold_progress_ring(ui, progress);
            });
    }
}

fn resolution(size:Option<(u32,u32)>)->String{size.map(|(w,h)|format!("{w}×{h}")).unwrap_or_else(||"—".to_owned())}
fn speed(bps:u64)->String{if bps>=1_000_000{format!("{:.2} Mbps",bps as f64/1_000_000.0)}else{format!("{:.1} kbps",bps as f64/1000.0)}}
fn millis(us:u64)->String{format!("{:.2} ms",us as f64/1000.0)}
fn optional_ms(us:u64,valid:bool)->String{if valid{millis(us)}else{"—".to_owned()}}
fn health(us:u64,good:u64,warn:u64)->egui::Color32{if us<=good{egui::Color32::from_rgb(126,226,165)}else if us<=warn{egui::Color32::from_rgb(250,209,116)}else{egui::Color32::from_rgb(255,139,139)}}
fn row(ui:&mut egui::Ui,label:&str,value:impl Into<String>,color:egui::Color32){
    ui.horizontal(|ui|{
        ui.label(egui::RichText::new(label).size(11.5).color(egui::Color32::from_gray(205)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{ui.label(egui::RichText::new(value.into()).size(11.5).color(color));});
    });
}
fn section(ui:&mut egui::Ui,label:&str){
    ui.add_space(3.0);ui.separator();ui.label(egui::RichText::new(label).size(11.0).strong().color(egui::Color32::from_rgb(126,226,165)));
}
fn show_diagnostics(ctx:&egui::Context,app:&App,streaming:&crate::app::stream_session::StreamingSession){
    let pt=app.settings.locale.as_str().starts_with("pt");
    let tr=|p:&'static str,e:&'static str|if pt{p}else{e};
    let white=egui::Color32::WHITE;let muted=egui::Color32::from_gray(160);
    egui::Area::new(egui::Id::new("stream-diagnostics-panel"))
        .order(egui::Order::Foreground).anchor(egui::Align2::RIGHT_TOP,egui::vec2(-9.0,9.0))
        .interactable(false).show(ctx,|ui|{
        let alpha=(u16::from(app.settings.hud_opacity.clamp(40,100))*255/100) as u8;
        egui::Frame::default().fill(egui::Color32::from_black_alpha(alpha))
            .stroke(egui::Stroke::new(1.0,egui::Color32::from_gray(65)))
            .corner_radius(6).inner_margin(8).show(ui,|ui|{
            ui.set_width(252.0);ui.spacing_mut().item_spacing.y=1.0;
            ui.label(egui::RichText::new(tr("DIAGNÓSTICO","DIAGNOSTICS")).size(13.0).strong().color(white));
            let Some(h)=streaming.hud.as_ref().filter(|h|h.at.elapsed()<std::time::Duration::from_secs(3)) else{
                ui.label(egui::RichText::new(tr("Aguardando métricas…","Waiting for measurements…")).size(11.5).color(muted));return;
            };
            let m=&h.playback;let input=crate::native_input::metrics();
            let fps=crate::streaming::hud::fps(m.presented,h.elapsed);
            let src=h.source_fps.map(|v|format!("{v:.0}")).unwrap_or_else(||"—".to_owned());
            row(ui,tr("Saída anunciada","Server advertised"),resolution(h.server.or(streaming.video_size())),white);
            row(ui,tr("Vídeo recebido","Received video"),resolution(h.encoded),white);
            row(ui,tr("Tela do Vita","Vita output"),if streaming.video_frame().is_some(){"960×544".to_owned()}else{"—".to_owned()},white);
            row(ui,tr("FPS servidor / Vita","FPS source / Vita"),format!("{src} / {fps:.0}"),white);
            row(ui,tr("Input local","Local input"),optional_ms(m.input_us,input.native_count+input.sdl_count>0),health(m.input_us,8000,15000));
            row(ui,tr("Dados (pipeline local)","Data (local pipeline)"),optional_ms(m.pipeline_us,m.presented>0),health(m.pipeline_us,50000,90000));
            row(ui,tr("Uso de rede (UDP)","Network (UDP)"),speed(h.rx_bps.saturating_add(h.tx_bps)),white);
            row(ui,"Download",speed(h.rx_bps),white);row(ui,"Upload",speed(h.tx_bps),white);
            row(ui,"Jitter",millis(h.jitter_us),health(h.jitter_us,10000,25000));
            match app.settings.hud_page.min(4){
                1=>{
                    section(ui,tr("REDE AVANÇADA","ADVANCED NETWORK"));
                    row(ui,tr("Vídeo recebido","Video payload"),speed(h.video_bps),white);
                    row(ui,tr("Bitrate solicitado","Requested bitrate"),speed(h.requested_bps),white);
                    row(ui,tr("Recebido / enviado","Received / sent"),format!("{:.1} / {:.1} MiB",h.rx_total as f64/1048576.0,h.tx_total as f64/1048576.0),white);
                    row(ui,tr("Datagramas RX / TX","Datagrams RX / TX"),format!("{} / {}",h.rx_packets,h.tx_packets),white);
                    row(ui,tr("Lacunas / atrasados","Gaps / late"),format!("{} / {}",h.gaps,h.late),white);
                    row(ui,tr("NACK pedidos / pacotes","NACK requests / packets"),format!("{} / {}",h.nacks,h.nack_packets),white);
                    row(ui,tr("Erros de socket","Socket errors"),h.socket_errors.to_string(),white);
                    row(ui,tr("RTT da sessão","Session RTT"),tr("Não medido","Not measured"),muted);
                },
                2=>{
                    section(ui,tr("VÍDEO E ÁUDIO","VIDEO & AUDIO"));
                    row(ui,tr("FPS decodificados","Decoded FPS"),format!("{:.1}",crate::streaming::hud::fps(m.decoded,h.elapsed)),white);
                    row(ui,tr("Montagem RTP méd / máx","RTP assembly avg / max"),format!("{:.1} / {:.1} ms",m.rtp_avg_us as f64/1000.0,m.rtp_max_us as f64/1000.0),white);
                    row(ui,tr("Decodificação","Decode"),millis(m.decode_us),white);
                    row(ui,tr("Idade da fila de vídeo","Video queue age"),millis(m.queue_age_us),white);
                    row(ui,tr("Espera de exibição","Display wait"),millis(m.display_us),white);
                    row(ui,tr("Descartes / erros","Drops / errors"),format!("{} / {}",h.dropped,h.errors),white);
                    row(ui,tr("Resync / reset¹","Resync / reset¹"),format!("{} / {}",m.resyncs,m.resets),white);
                    row(ui,tr("Buffer de áudio","Audio target"),format!("{} ms",m.audio_ms),white);
                    row(ui,tr("Áudio falhas / perdas¹","Audio underrun / drops¹"),format!("{} / {}",m.underruns,m.audio_drops),white);
                },
                3=>{
                    section(ui,tr("CONTROLES","INPUT"));
                    row(ui,tr("Leitura ativa","Active input"),if input.native_active{tr("Nativa","Native")}else{"SDL2"},white);
                    row(ui,tr("Intervalo méd / máx¹","Poll avg / max¹"),if input.polls>0{format!("{:.2} / {:.2} ms",input.interval_us as f64/1000.0,input.max_interval_us as f64/1000.0)}else{"—".to_owned()},white);
                    row(ui,tr("Consultas nativas¹","Native samples¹"),input.polls.to_string(),white);
                    row(ui,tr("Amostras repetidas¹","Repeated samples¹"),format!("{}%",input.repeat_percent),white);
                    row(ui,tr("Despacho nativo méd¹","Native dispatch avg¹"),optional_ms(input.native_avg_us,input.native_count>0),white);
                    row(ui,tr("Despacho SDL2 méd¹","SDL2 dispatch avg¹"),optional_ms(input.sdl_avg_us,input.sdl_count>0),white);
                    row(ui,tr("Envios nativo / SDL¹","Native / SDL sends¹"),format!("{} / {}",input.native_count,input.sdl_count),white);
                    row(ui,tr("Transições perdidas¹","Edge drops¹"),input.edge_drops.to_string(),white);
                    row(ui,tr("Zona morta nativa","Native deadzone"),format!("{}%",app.settings.native_deadzone_percent),white);
                },
                4=>{
                    section(ui,tr("VISÃO TÉCNICA","TECHNICAL OVERVIEW"));
                    row(ui,tr("Vídeo / alvo","Video / target"),format!("{} / {}",speed(h.video_bps),speed(h.requested_bps)),white);
                    row(ui,tr("Gaps / atrasados","Gaps / late"),format!("{} / {}",h.gaps,h.late),white);
                    row(ui,tr("Descartes / erros","Drops / errors"),format!("{} / {}",h.dropped,h.errors),white);
                    row(ui,tr("Fila cheia / trocas","Queue full / replaced"),format!("{} / {}",m.queue_full,m.replaced),white);
                    row(ui,tr("Montagem / decode","Assembly / decode"),format!("{:.1} / {:.1} ms",m.rtp_avg_us as f64/1000.0,m.decode_us as f64/1000.0),white);
                    row(ui,tr("Buffer de áudio","Audio target"),format!("{} ms",m.audio_ms),white);
                    row(ui,tr("Recuperação IDR","IDR recovery"),if h.waiting{tr("Aguardando","Waiting")}else{tr("Normal","Normal")},white);
                    row(ui,tr("Controle","Input"),if input.native_active{tr("Nativo","Native")}else{"SDL2"},white);
                },
                _=>{}
            }
            if matches!(app.settings.hud_page,2|3){ui.label(egui::RichText::new(tr("¹ Desde a abertura do app","¹ Since app launch")).size(9.5).color(muted));}
            ui.label(egui::RichText::new(tr("Tempos locais; RTT não medido.","Local times; RTT not measured.")).size(9.5).color(muted));
        });
    });
}
