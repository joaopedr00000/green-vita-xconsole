//! Independent experimental settings and best-effort Vita scheduling/power.
use std::sync::atomic::{AtomicU32,Ordering};
static AUDIO_GAIN:AtomicU32=AtomicU32::new(100);
pub(crate) fn audio_gain_percent()->u32{AUDIO_GAIN.load(Ordering::Relaxed)}
static ACTIVE_TEXTURES:AtomicU32=AtomicU32::new(0);
pub(crate) fn record_texture(count:usize){ACTIVE_TEXTURES.store(count as u32,Ordering::Relaxed);}
pub(crate) fn texture_count()->u32{ACTIVE_TEXTURES.load(Ordering::Relaxed)}
pub(crate) fn texture_format()->&'static str{"BGR565"}
pub(crate) fn configure(s:&crate::settings::Settings){AUDIO_GAIN.store(s.lab_audio_gain_percent.clamp(100,150),Ordering::Relaxed);}
#[cfg(target_os="vita")]
fn pin(mask:u32,label:&'static str){let r=unsafe{vitasdk_sys::sceKernelChangeThreadCpuAffinityMask(vitasdk_sys::sceKernelGetThreadId(),mask as i32)};crate::diagnostics::step(label,if r>=0{"OK"}else{"FAILED"});}
pub(crate) fn pin_render(){#[cfg(target_os="vita")]pin(vitasdk_sys::SCE_KERNEL_CPU_MASK_USER_0,"lab_render_affinity");}
pub(crate) fn pin_input(){#[cfg(target_os="vita")]pin(vitasdk_sys::SCE_KERNEL_CPU_MASK_USER_0|vitasdk_sys::SCE_KERNEL_CPU_MASK_USER_2,"lab_raw_affinity");}
pub(crate) fn pin_audio(){#[cfg(target_os="vita")]pin(vitasdk_sys::SCE_KERNEL_CPU_MASK_USER_2,"lab_audio_affinity");}
pub(crate) struct PerformanceMode{#[cfg(target_os="vita")]old:[i32;4]}
impl PerformanceMode{
    pub fn engage()->Self{
        #[cfg(target_os="vita")]{
            use vitasdk_sys::*;
            let old=unsafe{[scePowerGetArmClockFrequency(),scePowerGetBusClockFrequency(),scePowerGetGpuClockFrequency(),scePowerGetGpuXbarClockFrequency()]};
            apply([444,222,222,166]);Self{old}
        }
        #[cfg(not(target_os="vita"))]Self{}
    }
}
#[cfg(target_os="vita")]
fn apply(clocks:[i32;4]){
    use vitasdk_sys::*;
    for (v,set) in clocks.into_iter().zip([scePowerSetArmClockFrequency,scePowerSetBusClockFrequency,scePowerSetGpuClockFrequency,scePowerSetGpuXbarClockFrequency] as [unsafe extern "C" fn(i32)->i32;4]){if v>0{let r=unsafe{set(v)};if r<0{crate::diagnostics::step("lab_clocks","SET_FAILED");}}}
}
impl Drop for PerformanceMode{fn drop(&mut self){#[cfg(target_os="vita")]apply(self.old);}}

pub(crate) fn decoder_name()->&'static str{"Public"}
