//! PS Vita hardware H.264 decoder (`sceVideodec`/`sceAvcdec`).
use super::memory::{CdramBlock, release_reserved_decoder_cdram};
use super::{DecoderConfig, VideoTextureTarget};
use anyhow::{Result, bail};
use std::os::raw::c_void;
use vitasdk_sys::*;

// The idea of reducing the reference frames came from MattKC on his Vanilla project
// Make sure to check it out, good content :)
const AVCDEC_NUM_REF_FRAMES: u32 = 1;
// AVCDEC and SDL's Vita GXM renderer both support RGB565 natively. At 960x544 this halves the
// decoder-to-texture traffic from roughly 2 MiB to 1 MiB per frame, which matters at 60 FPS.
const OUTPUT_BYTES_PER_PIXEL: u32 = 2;
const OUTPUT_PIXEL_FORMAT: u32 = SCE_AVCDEC_PIXELFORMAT_RGBA565 as u32;

struct AvcdecLibrary {
    module_loaded: bool,
}

impl AvcdecLibrary {
    fn initialize(width: u32, height: u32) -> Result<Self> {
        let module_loaded = unsafe {
            let loaded_before = sceSysmoduleIsLoaded(SCE_SYSMODULE_AVCDEC);
            let ret = sceSysmoduleLoadModule(SCE_SYSMODULE_AVCDEC);
            if ret >= 0 {
                true
            } else if ret as u32 == SCE_SYSMODULE_ERROR_INVALID_VALUE {
                eprintln!(
                    "sceSysmoduleLoadModule(SCE_SYSMODULE_AVCDEC=0x{SCE_SYSMODULE_AVCDEC:x}) returned {ret:#x}; continuing with SceVideodec imports; is_loaded_before={loaded_before:#x}",
                );
                false
            } else {
                bail!(
                    "sceSysmoduleLoadModule(SCE_SYSMODULE_AVCDEC=0x{SCE_SYSMODULE_AVCDEC:x}) failed: {ret:#x}; is_loaded_before={loaded_before:#x}",
                );
            }
        };

        let init_info = SceVideodecQueryInitInfoHwAvcdec {
            size: size_of::<SceVideodecQueryInitInfoHwAvcdec>() as u32,
            horizontal: width,
            vertical: height,
            numOfRefFrames: AVCDEC_NUM_REF_FRAMES,
            numOfStreams: 1,
        };
        let ret = unsafe { sceVideodecInitLibrary(SCE_VIDEODEC_TYPE_HW_AVCDEC, &init_info) };
        if ret < 0 {
            if module_loaded {
                unsafe {
                    sceSysmoduleUnloadModule(SCE_SYSMODULE_AVCDEC);
                }
            }
            bail!("sceVideodecInitLibrary failed: {ret:#x}");
        }

        Ok(Self { module_loaded })
    }
}

impl Drop for AvcdecLibrary {
    fn drop(&mut self) {
        unsafe {
            sceVideodecTermLibrary(SCE_VIDEODEC_TYPE_HW_AVCDEC);
            if self.module_loaded {
                sceSysmoduleUnloadModule(SCE_SYSMODULE_AVCDEC);
            }
        }
    }
}

struct AvcdecDecoder(SceAvcdecCtrl);

impl Drop for AvcdecDecoder {
    fn drop(&mut self) {
        unsafe {
            sceAvcdecDeleteDecoder(&mut self.0);
        }
    }
}

pub struct PublicDecoder {
    decoder: AvcdecDecoder,
    _frame_memory: CdramBlock,
    _library: AvcdecLibrary,
    width: u32,
    height: u32,
}

impl PublicDecoder {
    pub fn new(config: DecoderConfig) -> Result<Self> {
        unsafe {
            let library = AvcdecLibrary::initialize(config.decode_width, config.decode_height)?;

            let query = SceAvcdecQueryDecoderInfo {
                horizontal: config.decode_width,
                vertical: config.decode_height,
                numOfRefFrames: AVCDEC_NUM_REF_FRAMES,
            };
            let mut decoder_info = SceAvcdecDecoderInfo { frameMemSize: 0 };
            let ret = sceAvcdecQueryDecoderMemSize(
                SCE_VIDEODEC_TYPE_HW_AVCDEC,
                &query,
                &mut decoder_info,
            );
            if ret < 0 {
                bail!("sceAvcdecQueryDecoderMemSize failed: {ret:#x}");
            }
            release_reserved_decoder_cdram();
            let frame_memory =
                CdramBlock::allocate("xcloud_hw_video_frame", decoder_info.frameMemSize)?;
            let mut decoder_control = SceAvcdecCtrl {
                handle: 0,
                frameBuf: SceAvcdecBuf {
                    pBuf: frame_memory.ptr.cast(),
                    size: decoder_info.frameMemSize,
                },
            };
            let ret =
                sceAvcdecCreateDecoder(SCE_VIDEODEC_TYPE_HW_AVCDEC, &mut decoder_control, &query);
            if ret < 0 {
                bail!("sceAvcdecCreateDecoder failed: {ret:#x}");
            }
            let decoder = AvcdecDecoder(decoder_control);

            Ok(Self {
                decoder,
                _frame_memory: frame_memory,
                _library: library,
                width: config.output_width,
                height: config.output_height,
            })
        }
    }

    /// Decodes one Access Unit. Returns `false` if the hardware buffered it without producing a picture yet.
    pub fn decode(
        &mut self,
        access_unit: &[u8],
        direct_target: VideoTextureTarget,
        input_pts:u64,
    ) -> Result<Option<u64>> {
        unsafe {
            let au = SceAvcdecAu {
                pts: SceVideodecTimeStamp {
                    upper: (input_pts >> 32) as u32,
                    lower: input_pts as u32,
                },
                dts: SceVideodecTimeStamp {
                    upper: (input_pts >> 32) as u32,
                    lower: input_pts as u32,
                },
                es: SceAvcdecBuf {
                    pBuf: access_unit.as_ptr() as *mut c_void,
                    size: access_unit.len() as u32,
                },
            };

            let output_ptr = direct_target.ptr as *mut u8;
            let output_pitch = direct_target.pitch / OUTPUT_BYTES_PER_PIXEL;
            let output_capacity = direct_target.capacity;
            if direct_target.pitch % OUTPUT_BYTES_PER_PIXEL != 0 || output_pitch.checked_mul(self.height).and_then(|n|n.checked_mul(OUTPUT_BYTES_PER_PIXEL)).is_none_or(|n|n>output_capacity) {
                bail!("invalid BGR565 target pitch or capacity");
            }
            if output_pitch < self.width {
                bail!(
                    "direct video texture pitch {output_pitch} is smaller than {}",
                    self.width
                );
            }

            let mut picture = SceAvcdecPicture {
                size: size_of::<SceAvcdecPicture>() as u32,
                frame: SceAvcdecFrame {
                    pixelType: OUTPUT_PIXEL_FORMAT,
                    framePitch: output_pitch,
                    frameWidth: self.width,
                    frameHeight: self.height,
                    horizontalSize: self.width,
                    verticalSize: self.height,
                    frameCropLeftOffset: 0,
                    frameCropRightOffset: 0,
                    frameCropTopOffset: 0,
                    frameCropBottomOffset: 0,
                    opt: SceAvcdecFrameOption {
                        rgba: SceAvcdecFrameOptionRGBA {
                            alpha: 0xff,
                            cscCoefficient: 0,
                            reserved: [0; 14],
                        },
                    },
                    pPicture: [output_ptr.cast(), std::ptr::null_mut()],
                },
                info: std::mem::zeroed(),
            };
            let mut picture_ptr: *mut SceAvcdecPicture = &mut picture;
            let mut array_picture = SceAvcdecArrayPicture {
                numOfOutput: 0,
                numOfElm: 1,
                pPicture: &mut picture_ptr,
            };

            let ret = sceAvcdecDecode(&self.decoder.0, &au, &mut array_picture);
            if ret < 0 {
                bail!("sceAvcdecDecode failed: {ret:#x}");
            }
            if array_picture.numOfOutput == 0 {
                return Ok(None);
            }

            // `framePitch` is in pixels. Validate the complete byte size before trusting the
            // decoder-provided pitch to write into the SDL texture.
            let row_bytes = picture
                .frame
                .framePitch
                .checked_mul(OUTPUT_BYTES_PER_PIXEL)
                .ok_or_else(|| {
                    anyhow::anyhow!("video frame pitch overflow: {}", picture.frame.framePitch)
                })?;
            let output_len = row_bytes.checked_mul(self.height).ok_or_else(|| {
                anyhow::anyhow!("video output size overflow: {row_bytes} * {}", self.height)
            })?;
            if output_len > output_capacity {
                bail!(
                    "sceAvcdecDecode produced pitch requiring {output_len} bytes, but output buffer has {} bytes",
                    output_capacity
                );
            }
            Ok(Some(((picture.info.pts.upper as u64)<<32)|picture.info.pts.lower as u64))
        }
    }
}

// SAFETY: the CDRAM blocks and decoder handle have no thread affinity in the underlying SCE API -
// this is only ever moved once (into `VideoDecodeWorker`'s thread), never accessed concurrently.
unsafe impl Send for PublicDecoder {}

use std::time::Instant;
use crate::streaming::hud::DecodeStages;
use super::{decoder_internal,timing::Pending};
#[derive(Clone,Copy)]
pub(super) struct FrameTiming{pub enqueued:Instant,pub stages:DecodeStages,pub generation:u64}
enum Backend{Public(PublicDecoder),Internal(decoder_internal::HwVideoDecoder)}
pub struct HwVideoDecoder{backend:Backend,pending:Pending<(FrameTiming,Instant)>,seq:u64,no_output:u8}
impl HwVideoDecoder{
    pub fn new(config:DecoderConfig)->Result<Self>{
        if crate::experimental::internal_enabled(){
            match decoder_internal::HwVideoDecoder::new(config){
                Ok(d)=>{crate::diagnostics::step("lab_decoder","INTERNAL_LOW_DELAY");crate::experimental::record_decoder(true);return Ok(Self{backend:Backend::Internal(d),pending:Pending::new(),seq:0,no_output:0});}
                Err(e)=>{crate::diagnostics::failure("lab_decoder",&format!("{e:#}"));crate::experimental::disable_internal();}
            }
        }
        crate::experimental::record_decoder(false);
        crate::diagnostics::step("lab_decoder","PUBLIC");
        Ok(Self{backend:Backend::Public(PublicDecoder::new(config)?),pending:Pending::new(),seq:0,no_output:0})
    }
    pub fn is_internal(&self)->bool{matches!(self.backend,Backend::Internal(_))}
    pub fn decode(&mut self,data:&[u8],target:VideoTextureTarget,mut timing:FrameTiming)->Result<Option<FrameTiming>>{
        self.seq=self.seq.saturating_add(1);
        let pts=self.seq.saturating_mul(super::AU_PTS_STEP);
        let started=Instant::now();
        let (returned,read_us)=match &mut self.backend{
            Backend::Public(d)=>{
                let returned=d.decode(data,target,pts)?;
                timing.stages.decode_us=started.elapsed().as_micros() as u64;
                self.pending.push(pts,(timing,started));(returned,0)
            }
            Backend::Internal(d)=>{
                d.submit_access_unit(data)?;
                debug_assert_eq!(d.submitted_sequence(),self.seq);
                timing.stages.decode_us=started.elapsed().as_micros() as u64;
                self.pending.push(pts,(timing,started));
                let read=Instant::now();let returned=d.get_picture(target)?;
                (returned,read.elapsed().as_micros() as u64)
            }
        };
        let Some(id)=returned else{
            self.no_output=self.no_output.saturating_add(1);
            if self.is_internal()&&self.no_output>=24{bail!("internal AVCDEC: 24 submissions without output");}return Ok(None);
        };
        self.no_output=0;
        if let Some((mut original,submitted))=self.pending.take(id){
            original.stages.verified=true;original.stages.decoder_age_us=submitted.elapsed().as_micros() as u64;
            original.stages.decode_us=original.stages.decode_us.saturating_add(read_us);
            original.stages.decoder_pending=self.pending.len() as u32;Ok(Some(original))
        }else{
            timing.stages.verified=false;timing.stages.decoder_pending=self.pending.len() as u32;Ok(Some(timing))
        }
    }
}
