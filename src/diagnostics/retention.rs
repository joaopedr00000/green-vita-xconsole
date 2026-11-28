//! Limit diagnostics to the currently running build and its rotated segment.
use std::{fs,path::Path};

fn valid_build(id:&str)->bool{
    let n=id.strip_suffix("-local").unwrap_or(id);
    !n.is_empty() && n.bytes().all(|b|b.is_ascii_digit())
}

fn obsolete(name:&str,current:&str)->bool{
    if !valid_build(current){return false;}
    let name=name.strip_suffix(".previous").unwrap_or(name);
    if name=="connection-status.txt"{return true;}
    let Some(id)=name.strip_prefix("connection-status-b").and_then(|s|s.strip_suffix(".txt"))else{return false;};
    valid_build(id)&&id!=current
}

/// Called once from the logger thread, after a current log is writable.
/// Never recurse, follow links or touch files outside the exact allowlist.
pub(crate) fn prune(dir:&Path,current:&str)->(u64,u64){
    let Ok(entries)=fs::read_dir(dir)else{return(0,1);};
    let(mut removed,mut failed)=(0u64,0u64);
    for entry in entries{
        let Ok(entry)=entry else{failed+=1;continue;};
        if !entry.file_name().to_str().is_some_and(|s|obsolete(s,current)){continue;}
        match entry.file_type(){
            Ok(t) if t.is_file()=>match fs::remove_file(entry.path()){Ok(())=>removed+=1,Err(_)=>failed+=1},
            Ok(_)=>{},
            Err(_)=>failed+=1,
        }
    }
    (removed,failed)
}

#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn names_are_strict(){
        for name in ["connection-status.txt","connection-status.txt.previous","connection-status-b18.txt","connection-status-b18.txt.previous","connection-status-b16-local.txt"]{assert!(obsolete(name,"19"),"{name}");}
        for name in ["connection-status-b19.txt","connection-status-b19.txt.previous","connection-status-b.txt","connection-status-backup.txt","connection-status-b18.txt.bak","connection-status-b18.txt.previous.previous","token-vault.bin","settings.json","log.txt"]{assert!(!obsolete(name,"19"),"{name}");}
        assert!(!obsolete("connection-status-b18.txt","unknown"));
    }
    #[test]fn cleanup_preserves_current_and_other_files(){
        let dir=std::env::temp_dir().join(format!("green-vita-retention-{}",std::process::id()));
        fs::create_dir(&dir).unwrap();
        for name in ["connection-status-b18.txt","connection-status-b18.txt.previous","connection-status.txt","connection-status-b19.txt","connection-status-b19.txt.previous","settings.json"]{fs::write(dir.join(name),b"test").unwrap();}
        fs::create_dir(dir.join("connection-status-b17.txt")).unwrap();
        #[cfg(unix)]std::os::unix::fs::symlink(dir.join("settings.json"),dir.join("connection-status-b16.txt")).unwrap();
        assert_eq!(prune(&dir,"19"),(3,0));
        for name in ["connection-status-b19.txt","connection-status-b19.txt.previous","settings.json","connection-status-b17.txt"]{assert!(dir.join(name).exists());}
        #[cfg(unix)]assert!(fs::symlink_metadata(dir.join("connection-status-b16.txt")).unwrap().file_type().is_symlink());
        assert_eq!(prune(&dir,"19"),(0,0));
        fs::remove_dir_all(dir).unwrap();
    }
}
