//! Bounded render-thread benchmark; no per-frame I/O, sort or lock.
use std::sync::Mutex;
use std::time::Instant;
const WARMUP:u64=10;
const WINDOW:u64=60;
const CAP:usize=8192;
#[derive(Clone,Copy,Debug,Default)]
pub(crate) struct Distribution{pub count:u64,pub min:u64,pub avg:u64,pub median:u64,pub p95:u64,pub p99:u64,pub max:u64,pub omitted:u64}
#[derive(Default)]
struct Samples{values:Vec<u64>,count:u64,sum:u64,min:u64,max:u64}
impl Samples{
    fn prepare(&mut self){self.values.reserve(CAP);}
    fn add(&mut self,v:u64){self.count+=1;self.sum=self.sum.saturating_add(v);self.min=if self.count==1{v}else{self.min.min(v)};self.max=self.max.max(v);if self.values.len()<CAP{self.values.push(v);}}
    fn finish(&mut self)->Distribution{
        self.values.sort_unstable();
        let n=self.values.len();
        let rank=|p:usize|if n==0{0}else{self.values[(n*p).div_ceil(100)-1]};
        Distribution{count:self.count,min:self.min,avg:self.sum.checked_div(self.count).unwrap_or(0),median:rank(50),p95:rank(95),p99:rank(99),max:self.max,omitted:self.count.saturating_sub(n as u64)}
    }
}
#[derive(Clone,Copy,Debug,Default)]
pub(crate) struct Report{pub phase:u8,pub seconds:u64,pub mode:u8,pub profile:u8,pub pipeline:Distribution,pub gap:Distribution,pub claim:Distribution,pub direct_wakes:u64}
static STATUS:Mutex<Report>=Mutex::new(Report{phase:0,seconds:0,mode:0,profile:0,pipeline:Distribution{count:0,min:0,avg:0,median:0,p95:0,p99:0,max:0,omitted:0},gap:Distribution{count:0,min:0,avg:0,median:0,p95:0,p99:0,max:0,omitted:0},claim:Distribution{count:0,min:0,avg:0,median:0,p95:0,p99:0,max:0,omitted:0},direct_wakes:0});
pub(crate) fn status()->Report{STATUS.lock().map(|s|*s).unwrap_or_default()}
fn publish(r:Report){if let Ok(mut s)=STATUS.lock(){*s=r;}}
#[derive(Default)]
pub(crate) struct Benchmark{key:Option<(u8,u8)>,start:Option<Instant>,pipeline:Samples,gap:Samples,claim:Samples,last_status_sec:Option<u64>,done:bool,direct_wakes:u64}
impl Benchmark{
    pub(crate) fn reset(&mut self){
        if self.key.is_some(){*self=Self::default();publish(Report::default());}
    }
    pub(crate) fn tick(&mut self,now:Instant,active:bool,mode:u8,profile:u8)->Option<Report>{
        if !active{self.reset();return None;}
        if self.key!=Some((mode,profile)){
            self.reset();self.key=Some((mode,profile));
            // Allocate before playback measurement begins, never in sample collection.
            self.pipeline.prepare();self.gap.prepare();self.claim.prepare();
            publish(Report{phase:1,mode,profile,..Report::default()});
        }
        let Some(start)=self.start else{return None;};
        if self.done{return None;}
        let elapsed=now.saturating_duration_since(start).as_secs();
        if elapsed>=WARMUP+WINDOW{
            self.done=true;
            let report=Report{phase:3,seconds:WINDOW,mode,profile,pipeline:self.pipeline.finish(),gap:self.gap.finish(),claim:self.claim.finish(),direct_wakes:self.direct_wakes};
            publish(report);return Some(report);
        }
        if self.last_status_sec!=Some(elapsed){
            self.last_status_sec=Some(elapsed);
            publish(Report{phase:if elapsed<WARMUP{1}else{2},seconds:if elapsed<WARMUP{elapsed}else{elapsed-WARMUP},mode,profile,pipeline:Distribution{count:self.pipeline.count,..Distribution::default()},..Report::default()});
        }
        None
    }
    pub(crate) fn observe(&mut self,now:Instant,total:u64,gap:u64,claim:u64,direct:bool){
        if self.key.is_none()||self.done{return;}
        let start=*self.start.get_or_insert(now);
        let sec=now.saturating_duration_since(start).as_secs();
        if !(WARMUP..WARMUP+WINDOW).contains(&sec){return;}
        if direct{self.direct_wakes+=1;}
        self.pipeline.add(total);
        if self.pipeline.count>1 && gap>0{self.gap.add(gap);}
        self.claim.add(claim);
    }
}
#[cfg(test)]
mod tests{
    use super::*;use std::time::Duration;
    #[test]fn percentile_nearest_rank_and_exact_extrema(){
        let mut s=Samples::default();for n in (1..=100).rev(){s.add(n);}
        let r=s.finish();assert_eq!((r.count,r.min,r.avg,r.median,r.p95,r.p99,r.max,r.omitted),(100,1,50,50,95,99,100,0));
    }
    #[test]fn warmup_window_and_restart(){
        let t=Instant::now();let mut b=Benchmark::default();
        assert!(b.tick(t,true,2,2).is_none());b.observe(t,999,0,999,true);
        b.observe(t+Duration::from_secs(9),999,1,999,true);
        b.observe(t+Duration::from_secs(10),4,16,1,true);
        b.observe(t+Duration::from_secs(69),6,17,2,false);
        b.observe(t+Duration::from_secs(70),999,1,999,true);
        let r=b.tick(t+Duration::from_secs(70),true,2,2).unwrap();
        assert_eq!((r.pipeline.count,r.pipeline.avg,r.gap.count,r.claim.p99,r.direct_wakes),(2,5,1,2,1));
        assert!(b.tick(t+Duration::from_secs(71),true,2,2).is_none());
        b.tick(t+Duration::from_secs(72),false,2,2);assert!(b.start.is_none());
    }
    #[test]fn bounded_storage(){let mut s=Samples::default();s.prepare();for n in 0..CAP+5{s.add(n as u64);}let r=s.finish();assert_eq!(r.omitted,5);assert_eq!(r.max,(CAP+4)as u64);}
}
