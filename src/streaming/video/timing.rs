//! Bounded PTS mapping; never guesses frame identity.
use std::collections::VecDeque;
const CAP:usize=32;
pub(crate) struct Pending<T>{items:VecDeque<(u64,T)>}
impl<T> Pending<T>{
    pub fn new()->Self{Self{items:VecDeque::with_capacity(CAP)}}
    pub fn push(&mut self,id:u64,t:T){if self.items.len()==CAP{self.items.pop_front();}self.items.push_back((id,t));}
    pub fn take(&mut self,id:u64)->Option<T>{let i=self.items.iter().position(|(key,_)|*key==id)?;self.items.remove(i).map(|(_,v)|v)}
    pub fn len(&self)->usize{self.items.len()}
}
#[cfg(test)] mod tests{use super::*;
#[test]fn reordered(){let mut p=Pending::new();p.push(1500,10);p.push(3000,20);p.push(4500,30);assert_eq!(p.take(1500),Some(10));assert_eq!(p.take(4500),Some(30));assert_eq!(p.take(3000),Some(20));assert_eq!(p.take(1500),None);}
#[test]fn bounded(){let mut p=Pending::new();for i in 0..40{p.push(i,i);}assert_eq!(p.len(),CAP);assert_eq!(p.take(0),None);assert_eq!(p.take(u64::MAX),None);assert_eq!(p.take(39),Some(39));}}

/// Keep decoder input backed by an initialized 64-byte tail, while passing only
/// the original compressed byte count to the API. The owned AU is consumed once.
pub(crate) fn pad_access_unit(data:&mut Vec<u8>)->Result<u32,&'static str>{
    let size=data.len();
    if size==0 || size>u32::MAX as usize-64 {return Err("invalid AVC input size");}
    data.try_reserve(64).map_err(|_|"AVC input padding allocation failed")?;
    data.resize(size+64,0);
    Ok(size as u32)
}
#[cfg(test)]mod padding_tests{
    use super::*;
    #[test]fn length_excludes_initialized_tail(){
        let mut au=vec![0,0,0,1,0x65,0xff];
        let original=au.clone();
        assert_eq!(pad_access_unit(&mut au),Ok(6));
        assert_eq!(&au[..6],&original[..]);
        assert_eq!(au.len(),70);
        assert!(au[6..].iter().all(|&b|b==0));
    }
    #[test]fn empty_is_rejected(){assert!(pad_access_unit(&mut vec![]).is_err());}
}

// RTP video timestamps use a 90 kHz clock. Maintain monotonic decoder IDs,
// including RTP wraparound and discontinuities; never infer returned identity.
#[derive(Default)]
pub(crate) struct PtsClock{last:Option<u32>,pts:u64}
impl PtsClock{
    pub fn next(&mut self,rtp:u32,period_us:Option<u64>)->u64{
        let nominal=period_us.filter(|v|(8_000..=100_000).contains(v)).map(|v|v*90/1000).unwrap_or(1500);
        let step=self.last.replace(rtp).map(|old|u64::from(rtp.wrapping_sub(old))).filter(|d|(720..=9000).contains(d)).unwrap_or(nominal);
        self.pts=self.pts.saturating_add(step.max(1));self.pts
    }
}
/// Switch only after eight consistent RTP frame intervals; isolated gaps don't
/// change cadence. Intermediate rates retain the existing smoothed estimate.
#[derive(Default)]
pub(crate) struct Cadence{active:u8,candidate:u8,run:u8}
impl Cadence{
    pub fn observe(&mut self,us:u64)->Option<(u8,u8)>{
        let rate=match us{13_000..=20_000=>60,27_000..=40_000=>30,_=>0};
        if rate==0 || rate==self.active{self.candidate=0;self.run=0;return None;}
        if self.candidate!=rate{self.candidate=rate;self.run=1;}else{self.run=self.run.saturating_add(1);}
        if self.run<8{return None;}
        let old=self.active;self.active=rate;self.run=0;Some((old,rate))
    }
    pub fn period(&self,fallback:Option<u64>)->Option<u64>{match self.active{30=>Some(33_333),60=>Some(16_667),_=>fallback}}
}
/// Restore 0.3.4's normal 2–6 AU capacity. A new decoder or recovery
/// keyframe gets all six slots so startup/IDR bursts can make progress.
pub(crate) fn queue_limit(period:Option<u64>,primed:bool,clean_idr:bool)->usize{
    if !primed || clean_idr{return 6;}
    let fps=period.filter(|v|*v>0).map(|v|1_000_000/v).unwrap_or(30);
    2+((fps.saturating_sub(30).min(25)*4+12)/25) as usize
}
/// Saturation is not a decoder failure. Keep the current generation and
/// drain its already accepted prefix; do not resume dependent pictures until IDR.
#[derive(Default)]
pub(crate) struct QueueRecovery{waiting:bool}
impl QueueRecovery{
    pub fn accepts(&self,idr:bool)->bool{!self.waiting||idr}
    pub fn rejected(&mut self)->bool{let first=!self.waiting;self.waiting=true;first}
    pub fn accepted(&mut self,idr:bool)->bool{if self.waiting&&idr{self.waiting=false;true}else{false}}
    pub fn waiting(&self)->bool{self.waiting}
    pub fn clear(&mut self){self.waiting=false;}
}
pub(crate) fn has_vcl(mask:u32)->bool{mask&((0b11111<<1)|(0b111<<19))!=0}
#[cfg(test)]mod cadence_tests{
    use super::*;
    #[test]fn pts_tracks_rates_wrap_and_discontinuity(){
        let mut c=PtsClock::default();
        let a=c.next(u32::MAX-999,Some(16_667));
        let b=c.next(500,Some(16_667));assert_eq!(b-a,1500);
        let d=c.next(3500,Some(33_333));assert_eq!(d-b,3000);
        let e=c.next(100_000,Some(33_333));assert_eq!(e-d,2999);
        assert!(c.next(100_000,Some(33_333))>e);
    }
    #[test]fn hysteresis_and_queue_budget(){
        let mut c=Cadence::default();
        for _ in 0..7{assert_eq!(c.observe(16_667),None);}
        assert_eq!(c.observe(16_667),Some((0,60)));
        assert_eq!(c.observe(33_333),None);assert_eq!(c.observe(16_667),None);
        for _ in 0..7{assert_eq!(c.observe(33_333),None);}
        assert_eq!(c.observe(33_333),Some((60,30)));
        assert_eq!(c.period(None),Some(33_333));
        for _ in 0..7{assert_eq!(c.observe(16_667),None);}
        assert_eq!(c.observe(16_667),Some((30,60)));
        assert_eq!(queue_limit(Some(33_333),true,false),2);assert_eq!(queue_limit(Some(16_667),true,false),6);
        assert_eq!(queue_limit(None,true,false),2);assert_eq!(queue_limit(Some(0),true,false),2);
        assert_eq!(queue_limit(Some(33_333),false,false),6);assert_eq!(queue_limit(Some(33_333),true,true),6);
    }
    #[test]fn overflow_waits_for_accepted_idr_without_restart(){
        let mut r=QueueRecovery::default();
        assert!(r.accepts(false));assert!(r.rejected());assert!(!r.rejected());
        assert!(!r.accepts(false));assert!(r.accepts(true));
        assert!(!r.accepted(false));assert!(r.waiting());
        assert!(!r.rejected());assert!(r.waiting());
        assert!(r.accepted(true));assert!(!r.waiting());assert!(r.accepts(false));
        assert!(r.rejected());r.clear();assert!(r.accepts(false));
    }
    #[test]fn nal_classification(){assert!(!has_vcl((1<<7)|(1<<8)));assert!(has_vcl(1<<5));assert!(has_vcl(1<<1));}
}

#[derive(Default)]pub(crate) struct CallClass{pub n:u64,pub sum:u64,pub max:u64,pub slow:u64}
impl CallClass{fn record(&mut self,us:u64){self.n+=1;self.sum=self.sum.saturating_add(us);self.max=self.max.max(us);if us>16_000{self.slow+=1;}}pub fn avg(&self)->u64{self.sum/self.n.max(1)}}
#[derive(Default)]pub(crate) struct CallStats{pub picture:CallClass,pub empty:CallClass}
impl CallStats{pub fn record(&mut self,picture:bool,us:u64){if picture{self.picture.record(us)}else{self.empty.record(us)}}}
#[cfg(test)]#[test]fn call_classes_are_independent(){let mut s=CallStats::default();s.record(true,2000);s.record(true,3000);s.record(false,18000);assert_eq!(s.picture.avg(),2500);assert_eq!(s.picture.max,3000);assert_eq!(s.picture.slow,0);assert_eq!(s.empty.avg(),18000);assert_eq!(s.empty.slow,1);}
