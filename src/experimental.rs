//! Independent experimental settings and best-effort Vita scheduling/power.
use std::sync::atomic::{AtomicBool,Ordering};
static TRIPLE:AtomicBool=AtomicBool::new(true);
static INTERNAL:AtomicBool=AtomicBool::new(true);
pub(crate) fn configure(s:&crate::settings::Settings){TRIPLE.store(s.lab_triple_buffer,Ordering::Relaxed);INTERNAL.store(s.lab_internal_decoder,Ordering::Relaxed);}
pub(crate) fn triple_enabled()->bool{TRIPLE.load(Ordering::Relaxed)}
pub(crate) fn internal_enabled()->bool{INTERNAL.load(Ordering::Relaxed)}
#[cfg(target_os="vita")]
fn pin(mask:u32,label:&str){let r=unsafe{vitasdk_sys::sceKernelChangeThreadCpuAffinityMask(vitasdk_sys::sceKernelGetThreadId(),mask as i32)};crate::diagnostics::step(label,if r>=0{"OK"}else{"FAILED"});}
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
