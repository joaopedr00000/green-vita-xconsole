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

/// NAL types that contain coded slices (including extension types).
/// Diagnostic classification only; no input bytes are filtered or changed.
pub(crate) fn has_vcl(mask:u32)->bool{
    const VCL_MASK:u32=(0b11111<<1)|(0b111<<19);
    mask&VCL_MASK!=0
}
#[cfg(test)]mod nal_class_tests{
    use super::*;
    #[test]fn vcl_vs_headers(){
        assert!(!has_vcl((1<<7)|(1<<8)|(1<<6)|(1<<9)));
        assert!(has_vcl((1<<7)|(1<<8)|(1<<5)));
        assert!(has_vcl(1<<1));
        assert!(has_vcl(1<<20));
        assert!(!has_vcl(0));
    }
}
