#![allow(dead_code, non_snake_case)]
extern crate self as hbb_common;
extern crate self as virtual_display;
pub use anyhow::bail;
pub use log;
pub type ResultType<T> = anyhow::Result<T>;
#[macro_export]
macro_rules! allow_err { ($value:expr) => { if let Err(e) = $value { eprintln!("{e}"); } }; }
pub struct MonitorMode { pub width: u32, pub height: u32, pub sync: u32 }
pub mod config {
    pub struct Config;
    impl Config { pub fn get_option(_: &str) -> String { String::new() } pub fn set_option(_: String, _: String) {} }
}
mod display_service { pub fn temp_ignore_displays_changed() {} }
mod platform {
    pub fn is_physical_console_session() -> Option<bool> { Some(!crate::fakes::STATE.lock().unwrap().remote) }
    pub fn change_resolution(_: &str, _: u32, _: u32) -> crate::ResultType<()> { Ok(()) }
    pub mod windows { pub mod reg_display_settings {
        pub type RegRecovery = Vec<u8>;
        pub fn read_reg_connectivity() -> crate::ResultType<Vec<u8>> { Ok(vec![]) }
        pub fn diff_recent_connectivity(_: Vec<u8>, _: Vec<u8>) -> Option<Vec<u8>> { None }
        pub fn restore_reg_connectivity(_: Vec<u8>, _: bool) -> crate::ResultType<()> { Ok(()) }
    }}
}
mod virtual_display_manager {
    pub fn get_cur_device_string() -> &'static str { "USB Mobile Monitor Virtual Display\0" }
    pub fn is_amyuni_idd() -> bool { true }
    pub fn is_virtual_display_supported() -> bool { true }
    pub fn plug_in_peer_request(_: Vec<Vec<crate::MonitorMode>>) -> crate::ResultType<Vec<u32>> {
        let mut s=crate::fakes::STATE.lock().unwrap(); s.plugs+=1;
        if s.fail_plug { anyhow::bail!("synthetic driver error"); }
        s.created=true;
        if s.disturb { s.physical[1]=true; }
        if s.driver==1 { s.virtual_active=true; }
        Ok(vec![0])
    }
    pub fn plug_out_monitor_indices(_: &[u32], _: bool, _: bool) -> crate::ResultType<()> {
        let mut s=crate::fakes::STATE.lock().unwrap(); s.unplugs+=1; s.created=false; s.virtual_active=false; Ok(())
    }
    pub fn plug_out_monitor(_: i32, _: bool, _: bool) -> crate::ResultType<()> { anyhow::bail!("unexpected global unplug") }
}
mod privacy_mode {
    pub const INVALID_PRIVACY_MODE_CONN_ID:i32=0;
    pub const NO_PHYSICAL_DISPLAYS:&str="no physical displays";
    pub const PRIVACY_MODE_IMPL_WIN_VIRTUAL_DISPLAY:&str="privacy_mode_impl_virtual_display";
    pub enum PrivacyModeState { OffSucceeded }
    pub trait PrivacyMode {
        fn is_async_privacy_mode(&self)->bool;
        fn init(&self)->crate::ResultType<()>;
        fn clear(&mut self);
        fn turn_on_privacy(&mut self,id:i32)->crate::ResultType<bool>;
        fn turn_off_privacy(&mut self,id:i32,state:Option<PrivacyModeState>)->crate::ResultType<()>;
        fn pre_conn_id(&self)->i32;
        fn get_impl_key(&self)->&str;
        fn check_on_conn_id(&self,id:i32)->crate::ResultType<bool> { Ok(self.pre_conn_id()==id) }
        fn check_off_conn_id(&self,_:i32)->crate::ResultType<()> { Ok(()) }
    }
    pub fn set_privacy_mode_state(_:i32,_:PrivacyModeState,_:String,_:u64)->crate::ResultType<()> { Ok(()) }
    pub mod win_input {
        pub fn hook()->crate::ResultType<()> { Ok(()) }
        pub fn unhook()->crate::ResultType<()> { Ok(()) }
    }
    pub mod win_virtual_display;
}
mod fakes {
    use std::sync::{LazyLock,Mutex};
    use winapi::{shared::{minwindef::{BOOL,DWORD},windef::HWND},um::wingdi::{DEVMODEW,DISPLAY_DEVICEW}};
    pub struct State {
        pub physical:[bool;2],pub virtual_active:bool,pub created:bool,pub driver:u8,
        pub remote:bool,pub disturb:bool,pub fail_plug:bool,pub fail_capture:bool,pub fail_restore:bool,
        pub plugs:u32,pub unplugs:u32,pub attaches:u32,pub restores:u32,pub query_race:bool,pub queries:u32,
    }
    impl Default for State { fn default()->Self { Self { physical:[true,false],virtual_active:false,created:false,driver:1,remote:false,disturb:false,fail_plug:false,fail_capture:false,fail_restore:false,plugs:0,unplugs:0,attaches:0,restores:0,query_race:false,queries:0 } } }
    pub static STATE:LazyLock<Mutex<State>>=LazyLock::new(||Mutex::new(State::default()));
    pub unsafe fn wide(p:*const u16)->String { let mut n=0;while *p.add(n)!=0 {n+=1;} String::from_utf16_lossy(std::slice::from_raw_parts(p,n)) }
    fn put(out:&mut [u16],text:&str){for (a,b) in out.iter_mut().zip(text.encode_utf16()){*a=b;}}
    pub unsafe fn EnumDisplayDevicesW(parent:*const u16,index:DWORD,out:*mut DISPLAY_DEVICEW,_:DWORD)->BOOL {
        let s=STATE.lock().unwrap(); let mut d:DISPLAY_DEVICEW=std::mem::zeroed();d.cb=std::mem::size_of_val(&d) as _;
        if parent.is_null(){
            if index>2{return 0;}
            put(&mut d.DeviceName,&format!("DISPLAY{index}"));
            put(&mut d.DeviceString,if index==2 {"USB Mobile Monitor Virtual Display"}else{"Physical display"});
            let active=if index==2 {s.virtual_active}else{s.physical[index as usize]};
            d.StateFlags=if active {1}else{0}; if index==0 && active {d.StateFlags|=4;}
        }else{
            let name=wide(parent);
            if index!=0 || (name=="DISPLAY2" && (!s.created || s.driver==0)){return 0;}
            put(&mut d.DeviceName,"MONITOR");d.StateFlags=3;
        }
        *out=d;1
    }
    pub unsafe fn EnumDisplaySettingsExW(name:*const u16,index:DWORD,out:*mut DEVMODEW,_:DWORD)->BOOL {
        if index<0xfffffffe && index>0{return 0;}
        let s=STATE.lock().unwrap();let virtual_display=wide(name)=="DISPLAY2";
        if virtual_display && (!s.created || s.driver==0){return 0;}
        (*out).dmPelsWidth=1920;(*out).dmPelsHeight=1080;(*out).dmDisplayFrequency=60;
        (*out).u1.s2_mut().dmPosition.x=0;(*out).u1.s2_mut().dmPosition.y=0;1
    }
    pub unsafe fn EnumDisplaySettingsW(n:*const u16,i:DWORD,d:*mut DEVMODEW)->BOOL { EnumDisplaySettingsExW(n,i,d,0) }
    pub unsafe fn ChangeDisplaySettingsExW(name:*const u16,mode:*mut DEVMODEW,_:HWND,_:DWORD,_:*mut std::ffi::c_void)->i32 {
        let mut s=STATE.lock().unwrap();if name.is_null(){return 0;}
        let name=wide(name);let active=(*mode).dmPelsWidth>0 && (*mode).dmPelsHeight>0;
        if name=="DISPLAY2" {s.attaches+=1;s.virtual_active=active;}
        if name=="DISPLAY0" {s.physical[0]=active;}
        if name=="DISPLAY1" {s.physical[1]=active;}
        0
    }
}
mod fake_winapi {
    pub mod shared { pub use winapi::shared::*; }
    pub mod um {
        pub mod wingdi { pub use winapi::um::wingdi::*; }
        pub mod winuser {
            pub use winapi::um::winuser::{CDS_NORESET,CDS_RESET,CDS_SET_PRIMARY,CDS_UPDATEREGISTRY,DISP_CHANGE_FAILED,DISP_CHANGE_SUCCESSFUL,EDD_GET_DEVICE_INTERFACE_NAME,ENUM_CURRENT_SETTINGS,ENUM_REGISTRY_SETTINGS};
            pub use crate::fakes::{ChangeDisplaySettingsExW,EnumDisplayDevicesW,EnumDisplaySettingsExW,EnumDisplaySettingsW};
        }
    }
}
mod fake_windows {
    pub mod Win32 {
        pub mod Foundation { pub use windows::Win32::Foundation::*; }
        pub mod Devices { pub mod Display {
            pub use windows::Win32::Devices::Display::{DISPLAYCONFIG_MODE_INFO,DISPLAYCONFIG_PATH_INFO,DISPLAYCONFIG_TOPOLOGY_ID,QUERY_DISPLAY_CONFIG_FLAGS,SET_DISPLAY_CONFIG_FLAGS,QDC_ONLY_ACTIVE_PATHS,SDC_APPLY,SDC_SAVE_TO_DATABASE,SDC_USE_SUPPLIED_DISPLAY_CONFIG};
            use windows::Win32::Foundation::{WIN32_ERROR,ERROR_SUCCESS,ERROR_ACCESS_DENIED,ERROR_INSUFFICIENT_BUFFER};
            pub unsafe fn GetDisplayConfigBufferSizes(_:QUERY_DISPLAY_CONFIG_FLAGS,p:*mut u32,m:*mut u32)->WIN32_ERROR {
                let s=crate::fakes::STATE.lock().unwrap();if s.fail_capture{return ERROR_ACCESS_DENIED;}*p=s.physical.iter().filter(|a|**a).count() as u32;*m=*p;ERROR_SUCCESS
            }
            pub unsafe fn QueryDisplayConfig(_:QUERY_DISPLAY_CONFIG_FLAGS,p:*mut u32,paths:*mut DISPLAYCONFIG_PATH_INFO,m:*mut u32,modes:*mut DISPLAYCONFIG_MODE_INFO,_:Option<*mut DISPLAYCONFIG_TOPOLOGY_ID>)->WIN32_ERROR {
                let mut s=crate::fakes::STATE.lock().unwrap();s.queries+=1;if s.query_race && s.queries==1{return ERROR_INSUFFICIENT_BUFFER;}
                let mut n=0;for (id,active) in s.physical.iter().enumerate(){if *active {let mut path=DISPLAYCONFIG_PATH_INFO::default();path.sourceInfo.id=id as u32;*paths.add(n)=path;*modes.add(n)=DISPLAYCONFIG_MODE_INFO::default();n+=1;}}
                *p=n as u32;*m=n as u32;ERROR_SUCCESS
            }
            pub unsafe fn SetDisplayConfig(paths:Option<&[DISPLAYCONFIG_PATH_INFO]>,_:Option<&[DISPLAYCONFIG_MODE_INFO]>,flags:SET_DISPLAY_CONFIG_FLAGS)->i32 {
                let mut s=crate::fakes::STATE.lock().unwrap();s.restores+=1;if s.fail_restore{return ERROR_ACCESS_DENIED.0 as i32;}
                assert_eq!(flags,SDC_APPLY|SDC_USE_SUPPLIED_DISPLAY_CONFIG|SDC_SAVE_TO_DATABASE);
                s.physical=[false,false];for path in paths.unwrap() {s.physical[path.sourceInfo.id as usize]=true;}0
            }
        }}
    }
}
fn main() {
    use privacy_mode::{PrivacyMode,win_virtual_display::PrivacyModeImpl};
    let mut cases=Vec::new();
    for name in ["active virtual screen", "inactive virtual monitor activation", "no monitor preserves single physical screen", "backend error rolls back", "RDP rejected before driver request", "snapshot failure before driver request", "hotplug size race", "two physical screens preserved", "restore failure retained"] {
        *fakes::STATE.lock().unwrap()=fakes::State::default();
        {let mut s=fakes::STATE.lock().unwrap();match name {
            "inactive virtual monitor activation"=>s.driver=2,
            "no monitor preserves single physical screen"=>{s.driver=0;s.disturb=true;},
            "backend error rolls back"=>s.fail_plug=true,
            "RDP rejected before driver request"=>s.remote=true,
            "snapshot failure before driver request"=>s.fail_capture=true,
            "hotplug size race"=>s.query_race=true,
            "two physical screens preserved"=>s.physical=[true,true],
            "restore failure retained"=>{s.driver=0;s.fail_restore=true;},_=>()
        }}
        let mut privacy=PrivacyModeImpl::new("privacy_mode_impl_virtual_display");
        let result=privacy.turn_on_privacy(7);
        let success=matches!(name,"active virtual screen"|"inactive virtual monitor activation"|"hotplug size race"|"two physical screens preserved");
        assert_eq!(result.is_ok(),success,"{name}: {result:?}");
        if success { assert_eq!(privacy.pre_conn_id(),7);privacy.turn_off_privacy(7,None).unwrap(); }
        let s=fakes::STATE.lock().unwrap();
        if name=="restore failure retained" {assert_eq!(s.restores,1);drop(s);assert!(privacy.turn_on_privacy(7).is_err());}
        else {assert_eq!(s.physical,if name=="two physical screens preserved"{[true,true]}else{[true,false]},"{name}");
            if name=="inactive virtual monitor activation" {assert!(s.attaches>0);}
            if name=="no monitor preserves single physical screen" {assert_eq!(s.attaches,0);assert_eq!(s.unplugs,1);}
            if name=="RDP rejected before driver request" || name=="snapshot failure before driver request" {assert_eq!(s.plugs,0);assert_eq!(s.restores,0);}
            if name=="hotplug size race" {assert_eq!(s.queries,2);}
        }
        cases.push(serde_json::json!({"name":name,"status":"passed"}));
    }
    println!("{}",serde_json::json!({"total":cases.len(),"passed":cases.len(),"failed":0,"skipped":0,"cases":cases}));
}
