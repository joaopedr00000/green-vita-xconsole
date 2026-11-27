//! Bounded PTS mapping; never guesses frame identity.
use std::collections::VecDeque;
const CAP:usize=32;
pub(super) struct Pending<T>{items:VecDeque<(u64,T)>}
impl<T> Pending<T>{
    pub fn new()->Self{Self{items:VecDeque::with_capacity(CAP)}}
    pub fn push(&mut self,id:u64,t:T){if self.items.len()==CAP{self.items.pop_front();}self.items.push_back((id,t));}
    pub fn take(&mut self,id:u64)->Option<T>{let i=self.items.iter().position(|(key,_)|*key==id)?;self.items.remove(i).map(|(_,v)|v)}
    pub fn len(&self)->usize{self.items.len()}
}
#[cfg(test)] mod tests{use super::*;
#[test]fn reordered(){let mut p=Pending::new();p.push(1500,10);p.push(3000,20);p.push(4500,30);assert_eq!(p.take(1500),Some(10));assert_eq!(p.take(4500),Some(30));assert_eq!(p.take(3000),Some(20));assert_eq!(p.take(1500),None);}
#[test]fn bounded(){let mut p=Pending::new();for i in 0..40{p.push(i,i);}assert_eq!(p.len(),CAP);assert_eq!(p.take(0),None);assert_eq!(p.take(u64::MAX),None);assert_eq!(p.take(39),Some(39));}}
