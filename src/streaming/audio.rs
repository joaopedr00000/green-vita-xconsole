use anyhow::{Context, Result, bail};
use bytes::Bytes;
use crossbeam_channel::{Receiver,Sender,bounded,unbounded,select_biased};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use crate::settings::{StreamProfile, active_stream_profile};
use crate::streaming::video::metrics::METRICS;
use std::ptr::NonNull;
use std::thread::JoinHandle;

pub const AUDIO_SAMPLE_RATE: i32 = 48_000;
const AUDIO_CHANNELS: usize = 2;

const AUDIO_BYTES_PER_SECOND: u32 = AUDIO_SAMPLE_RATE as u32 * AUDIO_CHANNELS as u32 * 2;
// Keep audio close to live video; the previous 240ms cap made audible events
// noticeably late after jitter. Retain conservative startup headroom for 2.4GHz.
fn pcm_queue_cap_bytes(target_ms: u32) -> u32 {
    AUDIO_BYTES_PER_SECOND * super::policy::pcm_cap_ms(target_ms) / 1_000
}
fn baseline_ms() -> u32 {
    match active_stream_profile() { StreamProfile::LowLatency => 50, StreamProfile::Balanced => 60, StreamProfile::Quality => 80 }
}
const MAX_OPUS_FRAME_SAMPLES_PER_CHANNEL: usize = 5_760;


const OPUS_OK: i32 = 0;

#[repr(C)]
struct OpusDecoderState {
    _private: [u8; 0],
}

#[link(name = "opus", kind = "static")]
unsafe extern "C" {
    fn opus_decoder_create(
        sample_rate: i32,
        channels: i32,
        error: *mut i32,
    ) -> *mut OpusDecoderState;
    fn opus_decode(
        decoder: *mut OpusDecoderState,
        data: *const u8,
        length: i32,
        pcm: *mut i16,
        frame_size: i32,
        decode_fec: i32,
    ) -> i32;
    fn opus_decoder_destroy(decoder: *mut OpusDecoderState);
}

struct NativeOpusDecoder {
    state: NonNull<OpusDecoderState>,
}

unsafe impl Send for NativeOpusDecoder {}

impl NativeOpusDecoder {
    fn new() -> Result<Self> {
        let mut error = OPUS_OK;
        // SAFETY: libopus initializes and exclusively owns the returned opaque decoder state.
        let state =
            unsafe { opus_decoder_create(AUDIO_SAMPLE_RATE, AUDIO_CHANNELS as i32, &mut error) };
        if error != OPUS_OK {
            if !state.is_null() {
                // SAFETY: a non-null state returned by libopus must be released with this function.
                unsafe { opus_decoder_destroy(state) };
            }
            bail!("libopus failed to create a decoder: error {error}");
        }
        let state = NonNull::new(state).context("libopus returned a null decoder")?;
        Ok(Self { state })
    }

    fn decode(&mut self, packet: &[u8], pcm: &mut [i16]) -> Result<usize> {
        let packet_len = i32::try_from(packet.len()).context("Opus packet is too large")?;
        // SAFETY: `state` is a live decoder, and `pcm` has room for the maximum Opus frame.
        let decoded = unsafe {
            opus_decode(
                self.state.as_ptr(),
                packet.as_ptr(),
                packet_len,
                pcm.as_mut_ptr(),
                MAX_OPUS_FRAME_SAMPLES_PER_CHANNEL as i32,
                0,
            )
        };
        if decoded < OPUS_OK {
            bail!("libopus decode error {decoded}");
        }
        Ok(decoded as usize)
    }
}

impl NativeOpusDecoder{
    fn conceal(&mut self,frames:usize,pcm:&mut [i16])->Result<usize>{
        if !super::policy::valid_plc_duration(frames){bail!("invalid PLC duration");}
        let n=unsafe{opus_decode(self.state.as_ptr(),std::ptr::null(),0,pcm.as_mut_ptr(),frames as i32,0)};
        if n<0{bail!("Opus PLC error {n}");}Ok(n as usize)
    }
}
impl Drop for NativeOpusDecoder {
    fn drop(&mut self) {
        // SAFETY: this is the sole owner and the state has not previously been destroyed.
        unsafe { opus_decoder_destroy(self.state.as_ptr()) };
    }
}


/// A reassembled Xbox Opus packet. Retain RTP timing and actual loss
/// evidence from SampleBuilder; never assume NVIDIA's 10 ms packet duration.
pub(crate) struct AudioPacket{pub data:Bytes,pub timestamp:u32,pub missing:u16,pub assembled_at:Instant}

/// Dedicated SDL device, owned exclusively by the audio worker after opening.
/// SDL's queue API is thread-safe. Drop runs only after the worker stops.
struct Device{id:sdl2::sys::SDL_AudioDeviceID,period_bytes:u32,period_us:u64}
impl Device{
    fn open()->Result<Self>{
        use sdl2::sys::*;
        let desired=SDL_AudioSpec{freq:AUDIO_SAMPLE_RATE,format:AUDIO_S16LSB as SDL_AudioFormat,channels:AUDIO_CHANNELS as u8,silence:0,samples:1024,padding:0,size:0,callback:None,userdata:std::ptr::null_mut()};
        let mut obtained=desired;
        let id=unsafe{SDL_OpenAudioDevice(std::ptr::null(),0,&desired,&mut obtained,0)};
        if id==0{bail!("SDL could not open queued audio");}
        if obtained.freq!=AUDIO_SAMPLE_RATE || obtained.channels!=AUDIO_CHANNELS as u8 || obtained.format!=desired.format{
            unsafe{SDL_CloseAudioDevice(id);}bail!("unexpected SDL audio format");
        }
        let period_bytes=u32::from(obtained.samples.max(1))*AUDIO_CHANNELS as u32*2;
        let period_us=u64::from(obtained.samples.max(1))*1_000_000/AUDIO_SAMPLE_RATE as u64;
        crate::streaming::video::metrics::AUDIO_DEVICE_PERIOD_US.store(period_us,Ordering::Relaxed);
        Ok(Self{id,period_bytes,period_us})
    }
    fn size(&self)->u32{unsafe{sdl2::sys::SDL_GetQueuedAudioSize(self.id)}}
    fn pause(&self,pause:bool){unsafe{sdl2::sys::SDL_PauseAudioDevice(self.id,i32::from(pause));}}
    fn clear(&self){unsafe{sdl2::sys::SDL_ClearQueuedAudio(self.id);}}
    fn queue(&self,s:&[i16])->bool{unsafe{sdl2::sys::SDL_QueueAudio(self.id,s.as_ptr().cast(),std::mem::size_of_val(s) as u32)==0}}
}
impl Drop for Device{fn drop(&mut self){self.pause(true);self.clear();unsafe{sdl2::sys::SDL_CloseAudioDevice(self.id);}}}

enum Control{Bind(Receiver<AudioPacket>,Sender<()>),Clear(Sender<()>),Stop}
pub struct AudioRenderer{_audio:sdl2::AudioSubsystem,control:Sender<Control>,worker:Option<JoinHandle<()>>}
impl AudioRenderer{
    pub fn new(audio:&sdl2::AudioSubsystem)->Result<Self>{
        let device=Device::open()?;
        let decoder=NativeOpusDecoder::new()?;
        let (control,rx)=unbounded();
        let worker=std::thread::Builder::new().name("green-vita-audio-direct".into()).spawn(move||run(device,decoder,rx)).context("spawn audio worker")?;
        Ok(Self{_audio:audio.clone(),control,worker:Some(worker)})
    }
    pub fn bind_session(&mut self,source:Receiver<AudioPacket>){
        let (tx,rx)=bounded(1);
        if self.control.send(Control::Bind(source,tx)).is_ok() && rx.recv_timeout(Duration::from_millis(200)).is_err(){crate::diagnostics::step("lab_audio","BIND_TIMEOUT");}
    }
    pub fn clear_session(&mut self){
        let (tx,rx)=bounded(1);
        if self.control.send(Control::Clear(tx)).is_ok() && rx.recv_timeout(Duration::from_millis(200)).is_err(){crate::diagnostics::step("lab_audio","CLEAR_TIMEOUT");}
    }
}
impl Drop for AudioRenderer{fn drop(&mut self){let _=self.control.send(Control::Stop);if let Some(worker)=self.worker.take(){let _=worker.join();}}}

struct Playback{
    device:Device,decoder:NativeOpusDecoder,pcm:Vec<i16>,started:bool,fade:bool,target:u32,
    last_adjustment:Instant,last_underrun:Instant,empty_since:Option<Instant>,low_water:u32,
    last_arrival:Option<Instant>,arrival_gap_us:u64,previous:Option<(u32,usize)>,
}
impl Playback{
    fn new(device:Device,decoder:NativeOpusDecoder)->Self{
        Self{device,decoder,pcm:vec![0;MAX_OPUS_FRAME_SAMPLES_PER_CHANNEL*AUDIO_CHANNELS],started:false,fade:true,
            target:super::policy::audio_start_ms(crate::settings::latency_mode(),baseline_ms()),
            last_adjustment:Instant::now(),last_underrun:Instant::now(),empty_since:None,low_water:u32::MAX,
            last_arrival:None,arrival_gap_us:0,previous:None}
    }
    fn reset(&mut self){
        self.device.pause(true);self.device.clear();self.started=false;self.fade=true;
        self.target=super::policy::audio_start_ms(crate::settings::latency_mode(),baseline_ms());
        self.last_adjustment=Instant::now();self.last_underrun=Instant::now();
        self.empty_since=None;self.low_water=u32::MAX;self.last_arrival=None;self.arrival_gap_us=0;self.previous=None;
        // A fresh decoder prevents codec state from crossing session boundaries.
        if let Ok(d)=NativeOpusDecoder::new(){self.decoder=d;}else{crate::diagnostics::step("lab_audio","RESET_DECODER_FAILED");}
        use crate::streaming::video::metrics::*;
        for m in [&AUDIO_QUEUED_MS,&AUDIO_ARRIVAL_US,&AUDIO_TRIM_FRAMES,&AUDIO_REFILLS,&AUDIO_PLC_FRAMES,&AUDIO_HANDOFF_DROPS,&AUDIO_HANDOFF_MAX_US]{m.store(0,Ordering::Relaxed);}
        METRICS.audio_target_ms.store(self.target as u64,Ordering::Relaxed);
    }
    fn packet(&mut self,packet:AudioPacket){
        let now=Instant::now();
        crate::streaming::video::metrics::AUDIO_HANDOFF_MAX_US.fetch_max(now.saturating_duration_since(packet.assembled_at).as_micros() as u64,Ordering::Relaxed);
        if let Some(at)=self.last_arrival{let us=now.saturating_duration_since(at).as_micros().min(1_000_000) as u64;self.arrival_gap_us=(self.arrival_gap_us*7+us)/8;}
        self.last_arrival=Some(now);
        if self.started && self.device.size()==0 && self.empty_since.is_none(){self.empty_since=Some(now);self.fade=true;}
        // PLC only on bounded, timestamp-confirmed losses. Never synthesize long
        // silence gaps, wraparound anomalies or unconfirmed DTX intervals.
        if let Some((ts,frames))=self.previous{
            let count=super::policy::plc_packet_count(ts,packet.timestamp,frames,packet.missing);
            for _ in 0..count{
                self.pcm.resize(MAX_OPUS_FRAME_SAMPLES_PER_CHANNEL*AUDIO_CHANNELS,0);
                if let Ok(n)=self.decoder.conceal(frames,&mut self.pcm){if n>0{
                    self.pcm.truncate(n*AUDIO_CHANNELS);
                    if self.queue_pcm(false){crate::streaming::video::metrics::AUDIO_PLC_FRAMES.fetch_add(n as u64,Ordering::Relaxed);}
                }}
            }
        }
        self.pcm.resize(MAX_OPUS_FRAME_SAMPLES_PER_CHANNEL*AUDIO_CHANNELS,0);
        match self.decoder.decode(&packet.data,&mut self.pcm){
            Ok(n) if n>0=>{self.previous=Some((packet.timestamp,n));self.pcm.truncate(n*AUDIO_CHANNELS);self.queue_pcm(true);}
            _=>{METRICS.audio_drops.fetch_add(1,Ordering::Relaxed);self.fade=true;self.previous=None;
                if let Ok(d)=NativeOpusDecoder::new(){self.decoder=d;}
            }
        }
    }
    fn queue_pcm(&mut self,allow_trim:bool)->bool{
        let q=self.device.size();
        let queued_ms=q.saturating_mul(1000)/AUDIO_BYTES_PER_SECOND;
        let trim=super::policy::audio_rate_trim_frames(crate::settings::latency_mode(),allow_trim&&self.started&&self.empty_since.is_none(),queued_ms,self.target,self.pcm.len()/AUDIO_CHANNELS);
        let removed=if trim>0{super::policy::shorten_stereo_pcm(&mut self.pcm,trim)}else{0};
        if q.saturating_add(std::mem::size_of_val(self.pcm.as_slice()) as u32)>pcm_queue_cap_bytes(self.target){
            METRICS.audio_drops.fetch_add(1,Ordering::Relaxed);self.fade=true;return false;
        }
        let gain=crate::experimental::audio_gain_percent();
        if gain>100{super::policy::apply_audio_gain(&mut self.pcm,gain);}
        if self.fade{
            let fade_frames=AUDIO_SAMPLE_RATE as usize/200;
            for (i,sample) in self.pcm.iter_mut().take(fade_frames*AUDIO_CHANNELS).enumerate(){*sample=(i32::from(*sample)*(i/AUDIO_CHANNELS) as i32/fade_frames as i32) as i16;}
            self.fade=false;
        }
        if !self.device.queue(&self.pcm){METRICS.audio_drops.fetch_add(1,Ordering::Relaxed);self.fade=true;return false;}
        crate::streaming::video::metrics::AUDIO_TRIM_FRAMES.fetch_add(removed as u64,Ordering::Relaxed);
        if !self.started && self.device.size()>=AUDIO_BYTES_PER_SECOND*self.target/1000{self.device.pause(false);self.started=true;self.empty_since=None;}
        true
    }
    fn tick(&mut self){
        let q=self.device.size();let ms=q.saturating_mul(1000)/AUDIO_BYTES_PER_SECOND;
        if self.started{
            self.low_water=self.low_water.min(ms);
            if q==0 && self.empty_since.is_none(){self.empty_since=Some(Instant::now());self.fade=true;}
            if let Some(at)=self.empty_since{
                if q>=self.device.period_bytes{
                    self.empty_since=None;
                    crate::streaming::video::metrics::AUDIO_REFILLS.fetch_add(1,Ordering::Relaxed);
                }else if at.elapsed()>=Duration::from_micros(self.device.period_us.clamp(20_000,40_000)){
                    self.device.pause(true);self.started=false;self.empty_since=None;self.fade=true;
                    self.target=(self.target+10).min(110);self.last_underrun=Instant::now();self.last_adjustment=Instant::now();self.low_water=u32::MAX;
                    METRICS.audio_underruns.fetch_add(1,Ordering::Relaxed);
                }
            }
        }
        if self.last_underrun.elapsed()>=Duration::from_secs(30)&&self.last_adjustment.elapsed()>=Duration::from_secs(30){
            let floor=super::policy::audio_floor_ms(crate::settings::latency_mode(),baseline_ms());
            self.target=super::policy::audio_next_target_ms(self.target,floor,self.started,self.low_water);
            self.low_water=u32::MAX;self.last_adjustment=Instant::now();
        }
        METRICS.audio_target_ms.store(self.target as u64,Ordering::Relaxed);
        crate::streaming::video::metrics::AUDIO_QUEUED_MS.store(ms as u64,Ordering::Relaxed);
        crate::streaming::video::metrics::AUDIO_ARRIVAL_US.store(self.arrival_gap_us,Ordering::Relaxed);
    }
}
fn run(device:Device,decoder:NativeOpusDecoder,control:Receiver<Control>){
    crate::experimental::pin_audio();
    let mut playback=Playback::new(device,decoder);
    let mut source=crossbeam_channel::never::<AudioPacket>();
    loop{
        select_biased!{
            recv(control)->command=>match command{
                Ok(Control::Bind(next,ack))=>{
                    playback.reset();
                    // Discard only already stale packets from before the new binding.
                    while next.try_recv().is_ok(){}
                    source=next;let _=ack.send(());
                }
                Ok(Control::Clear(ack))=>{source=crossbeam_channel::never();playback.reset();let _=ack.send(());}
                Ok(Control::Stop)|Err(_)=>break,
            },
            recv(source)->packet=>match packet{
                Ok(packet)=>playback.packet(packet),
                Err(_)=>{source=crossbeam_channel::never();playback.reset();}
            },
            default(Duration::from_millis(4))=>{}
        }
        playback.tick();
    }
}
