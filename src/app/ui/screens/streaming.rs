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
// The Vita UI uses 1.3 physical pixels per point (544/1.3 = 418 logical
// points). egui widget minimum heights can make long diagnostic pages overflow.
// Draw noninteractive rows with an explicit height instead of clickable widgets.
const HUD_ROW_HEIGHT:f32=13.5;
fn row(ui:&mut egui::Ui,label:&str,value:impl Into<String>,color:egui::Color32){
    let value=value.into();
    let (rect,_)=ui.allocate_exact_size(egui::vec2(ui.available_width(),HUD_ROW_HEIGHT),egui::Sense::hover());
    let label_color=egui::Color32::from_gray(205);
    let font=egui::FontId::proportional(11.0);
    let left=ui.painter().layout_no_wrap(label.to_owned(),font.clone(),label_color);
    let right=ui.painter().layout_no_wrap(value.clone(),font,color);
    let needed=left.size().x+right.size().x+8.0;
    let (left,right)=if needed>rect.width(){
        let font=egui::FontId::proportional((11.0*(rect.width()-8.0)/(needed-8.0)).clamp(7.0,11.0));
        (ui.painter().layout_no_wrap(label.to_owned(),font.clone(),label_color),ui.painter().layout_no_wrap(value,font,color))
    }else{(left,right)};
    let painter=ui.painter().with_clip_rect(rect);
    painter.galley(egui::pos2(rect.left(),rect.center().y-left.size().y/2.0),left,label_color);
    painter.galley(egui::pos2(rect.right()-right.size().x,rect.center().y-right.size().y/2.0),right,color);
}
fn section(ui:&mut egui::Ui,label:&str){
    let (rect,_)=ui.allocate_exact_size(egui::vec2(ui.available_width(),17.0),egui::Sense::hover());
    let p=ui.painter();
    p.hline(rect.x_range(),rect.top()+1.0,egui::Stroke::new(1.0,egui::Color32::from_gray(75)));
    p.text(egui::pos2(rect.left(),rect.bottom()-1.0),egui::Align2::LEFT_BOTTOM,label,egui::FontId::proportional(11.0),egui::Color32::from_rgb(126,226,165));
}
fn note(ui:&mut egui::Ui,label:&str,color:egui::Color32){
    let (rect,_)=ui.allocate_exact_size(egui::vec2(ui.available_width(),12.0),egui::Sense::hover());
    ui.painter().with_clip_rect(rect).text(rect.left_center(),egui::Align2::LEFT_CENTER,label,egui::FontId::proportional(9.0),color);
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
            .corner_radius(6).inner_margin(6).show(ui,|ui|{
            ui.set_width(252.0);ui.spacing_mut().item_spacing.y=0.0;
            section(ui,tr("DIAGNÓSTICO","DIAGNOSTICS"));
            let Some(h)=streaming.hud.as_ref().filter(|h|h.at.elapsed()<std::time::Duration::from_secs(3)) else{
                note(ui,tr("Aguardando métricas…","Waiting for measurements…"),muted);return;
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
            row(ui,"RTT (ICE)",h.rtt.map(|r|format!("{}{}",millis(r.current_us),if r.age().as_secs()>30{" *"}else{""})).unwrap_or_else(||"—".to_owned()),h.rtt.filter(|r|r.age().as_secs()<=30).map(|r|health(r.current_us,50000,90000)).unwrap_or(muted));
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
                    row(ui,tr("RTT médio ICE","Mean ICE RTT"),h.rtt.map(|r|millis(r.average_us)).unwrap_or_else(||"—".to_owned()),white);
                    row(ui,tr("Respostas / idade obs.","Replies / observed age"),h.rtt.map(|r|format!("{} / {} s",r.responses,r.age().as_secs())).unwrap_or_else(||"—".to_owned()),white);
                },
                2=>{
                    section(ui,tr("VÍDEO E ÁUDIO","VIDEO & AUDIO"));
                    row(ui,tr("FPS decodificados","Decoded FPS"),format!("{:.1}",crate::streaming::hud::fps(m.decoded,h.elapsed)),white);
                    row(ui,tr("Montagem RTP méd / máx","RTP assembly avg / max"),format!("{:.1} / {:.1} ms",m.rtp_avg_us as f64/1000.0,m.rtp_max_us as f64/1000.0),white);
                    if let Some(s)=m.stages {
                        row(ui,tr("Fila comprimida","Compressed queue"),millis(s.decode.queue_us),white);
                        row(ui,tr("Espera textura","Texture wait"),millis(s.decode.texture_us),white);
                        row(ui,tr("Decodificação H.264","H.264 decode"),millis(s.decode.decode_us),white);
                        row(ui,tr("Espera exibição","Display wait"),millis(s.display_us),white);
                        row(ui,tr("Pipeline méd / máx","Pipeline avg / max"),if m.stage_samples>0{format!("{:.1} / {:.1} ms",m.pipeline_avg_us as f64/1000.0,m.pipeline_max_us as f64/1000.0)}else{"—".to_owned()},white);
                    }
                    row(ui,tr("Fila / trocas saída¹","Queue / output swaps¹"),format!("{} / {}",m.queue_depth,m.output_replacements),white);
                    row(ui,tr("Recuperação de fila¹","Queue catch-up¹"),format!("{} · {}",tr("Auto","Auto"),m.catch_up_replacements),white);
                    row(ui,tr("Descartes / erros","Drops / errors"),format!("{} / {}",h.dropped,h.errors),white);
                    row(ui,tr("Resync / reset¹","Resync / reset¹"),format!("{} / {}",m.resyncs,m.resets),white);
                    row(ui,tr("Buffer de áudio","Audio target"),format!("{} ms",m.audio_ms),white);
                    row(ui,tr("Áudio falhas / perdas¹","Audio underrun / drops¹"),format!("{} / {}",m.underruns,m.audio_drops),white);
                },
                3=>{
                    section(ui,tr("CONTROLES","INPUT"));
                    row(ui,tr("Leitura ativa","Active input"),if input.native_active{"Gamepad RAW"}else{"SDL2"},white);
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
                    row(ui,tr("Antes / chamada present","Before / present call"),m.stages.map(|s|format!("{:.1} / {:.1} ms",s.pre_present_us as f64/1000.0,s.present_call_us as f64/1000.0)).unwrap_or_else(||"—".to_owned()),white);
                    row(ui,tr("Buffer de áudio","Audio target"),format!("{} ms",m.audio_ms),white);
                    row(ui,tr("Recuperação IDR","IDR recovery"),if h.waiting{tr("Aguardando","Waiting")}else{tr("Normal","Normal")},white);
                    row(ui,tr("Controle","Input"),if input.native_active{"Gamepad RAW"}else{"SDL2"},white);
                },
                _=>{}
            }
            if matches!(app.settings.hud_page,2|3){note(ui,tr("¹ Contadores acumulados (catch-up: sessão)","¹ Cumulative counters (catch-up: session)"),muted);}
            note(ui,tr("RTT ICE ≠ input total; * amostra antiga.","ICE RTT ≠ total input; * old sample."),muted);
        });
    });
}
