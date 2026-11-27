//! Conservative H.264 SPS tail rewrite. No slice, reference count, profile, level
//! or colour field is altered. The caller must validate both SPS parses.
fn ue(value:u32)->Vec<bool>{
    let n=u64::from(value)+1; let width=64-n.leading_zeros() as usize;
    let mut bits=vec![false;width-1];
    bits.extend((0..width).rev().map(|i|n&(1u64<<i)!=0));bits
}
fn unescape(data:&[u8])->Option<Vec<u8>>{
    let mut out=Vec::with_capacity(data.len());let mut zeros=0;
    for (i,&b) in data.iter().enumerate(){
        if zeros>=2 && b==3 {
            if *data.get(i+1)?>3{return None;}
            zeros=0;continue;
        }
        if zeros>=2 && b<3{return None;}
        out.push(b);zeros=if b==0{zeros+1}else{0};
    }Some(out)
}
fn escape(data:&[u8])->Vec<u8>{
    let mut out=Vec::new();let mut zeros=0;
    for &b in data{
        if zeros>=2 && b<=3{out.push(3);zeros=0;}
        out.push(b);zeros=if b==0{zeros+1}else{0};
    }out
}
fn bits(data:&[u8])->Vec<bool>{data.iter().flat_map(|b|(0..8).rev().map(move |i|b&(1<<i)!=0)).collect()}
fn pack(mut b:Vec<bool>)->Vec<u8>{
    b.push(true);while b.len()%8!=0{b.push(false);}
    b.chunks(8).map(|c|c.iter().fold(0u8,|n,&v|(n<<1)|u8::from(v))).collect()
}
// The last two VUI restriction fields are ue(reorder) and ue(buffer).
// Without VUI/restrictions, the final syntax bit is their absent flag.
pub(super) fn rewrite(nal:&[u8],has_vui:bool,restriction:Option<(u32,u32)>,refs:u32,zero_reorder:bool)->Option<Vec<u8>>{
    if nal.len()<5||nal.len()>4096||nal[0]&0x9f!=7||refs!=1||!zero_reorder{return None;}
    let rbsp=unescape(&nal[1..])?;
    let mut b=bits(&rbsp);
    let stop=b.iter().rposition(|v|*v)?;
    if b.len()-stop>8{return None;}
    b.truncate(stop);
    if let Some((reorder,buffer))=restriction{
        if reorder!=0||buffer<=refs{return None;}
        let mut old=ue(reorder);old.extend(ue(buffer));
        if !b.ends_with(&old){return None;}
        b.truncate(b.len()-old.len());
        b.extend(ue(0));b.extend(ue(refs));
    }else{
        if b.pop()!=Some(false){return None;}
        if !has_vui{
            b.push(true); // vui_parameters_present_flag
            // aspect, overscan, signal, chroma, timing, nal HRD, vcl HRD, pic_struct
            b.extend([false;8]);
        }
        b.push(true); // bitstream_restriction_flag
        b.push(true); // motion vectors over picture boundaries
        for v in [2,1,16,16,0,refs]{b.extend(ue(v));}
    }
    let mut out=vec![nal[0]];out.extend(escape(&pack(b)));Some(out)
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn preserves_prefix_and_limits_buffer(){
        let mut b=vec![true,false,true,true,false];let prefix=b.clone();
        b.extend(ue(0));b.extend(ue(7));
        let mut n=vec![0x67];n.extend(escape(&pack(b)));
        let p=rewrite(&n,true,Some((0,7)),1,true).unwrap();
        let mut expected=prefix;expected.extend(ue(0));expected.extend(ue(1));
        assert_eq!(unescape(&p[1..]).unwrap(),pack(expected));
    }
    #[test]fn rejects_unknown_reorder_and_multiple_references(){
        let n=[0x67,0x42,0xe0,0x20,0x40];
        assert!(rewrite(&n,false,None,1,false).is_none());
        assert!(rewrite(&n,false,None,2,true).is_none());
        assert!(rewrite(&n,true,Some((2,7)),1,true).is_none());
        assert!(rewrite(&n,true,Some((0,1)),1,true).is_none());
    }
    #[test]fn adds_missing_restriction_without_changing_prefix(){
        let n=[0x67,0x42,0xe0,0x20,0x40];
        let p=rewrite(&n,true,None,1,true).unwrap();
        let mut expected=bits(&[0x42,0xe0,0x20]);expected.extend([true,true]);
        for v in [2,1,16,16,0,1]{expected.extend(ue(v));}
        assert_eq!(unescape(&p[1..]).unwrap(),pack(expected));
    }
    #[test]fn emulation_prevention_roundtrip(){
        let a=[0,0,0,0,1,0,0,2,0,0,3,4];
        assert_eq!(unescape(&escape(&a)).unwrap(),a);
    }
}
