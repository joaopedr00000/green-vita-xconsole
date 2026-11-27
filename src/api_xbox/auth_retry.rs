//! Bounded retry policy for startup token exchanges.
pub(crate) const MAX_ATTEMPTS:usize=3;
pub(crate) fn retry_delay_ms(attempt:usize,status:Option<u16>,retry_after:Option<&str>)->Option<u64>{
    if attempt==0 || attempt>=MAX_ATTEMPTS{return None;}
    if !matches!(status,None|Some(408|429|500|502|503|504)){return None;}
    let default=(if status==Some(429){2000u64}else{750u64})*(1u64<<(attempt-1));
    match retry_after{
        None=>Some(default),
        // Do not retry early if a server requests a longer delay or an HTTP-date.
        Some(raw)=>raw.trim().parse::<u64>().ok().filter(|seconds|*seconds<=10)
            .map(|seconds|default.max(seconds*1000)),
    }
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn bounded_transient_retry(){
        assert_eq!(retry_delay_ms(1,None,None),Some(750));
        assert_eq!(retry_delay_ms(2,Some(503),None),Some(1500));
        assert_eq!(retry_delay_ms(3,Some(503),None),None);
    }
    #[test]fn respects_rate_limits(){
        assert_eq!(retry_delay_ms(1,Some(429),None),Some(2000));
        assert_eq!(retry_delay_ms(2,Some(429),Some("8")),Some(8000));
        assert_eq!(retry_delay_ms(1,Some(429),Some("60")),None);
        assert_eq!(retry_delay_ms(1,Some(503),Some("Thu, 26 Nov 2026 23:30:00 GMT")),None);
    }
    #[test]fn permanent_errors_not_retried(){
        for status in [400,401,403,404,405,501]{assert_eq!(retry_delay_ms(1,Some(status),None),None);}
    }
}
