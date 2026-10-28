use hbb_common::{bail, log, ResultType};
use windows::Win32::{
    Devices::Display::{
        GetDisplayConfigBufferSizes, QueryDisplayConfig, SetDisplayConfig,
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, QDC_ONLY_ACTIVE_PATHS,
        SDC_APPLY, SDC_SAVE_TO_DATABASE, SDC_USE_SUPPLIED_DISPLAY_CONFIG,
    },
    Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS},
};

pub(super) struct DisplaySnapshot {
    paths: Vec<DISPLAYCONFIG_PATH_INFO>,
    modes: Vec<DISPLAYCONFIG_MODE_INFO>,
}

impl DisplaySnapshot {
    pub(super) fn capture() -> ResultType<Self> {
        // Hotplug can change the required buffer sizes between the two calls.
        for _ in 0..3 {
            let mut path_count = 0;
            let mut mode_count = 0;
            let rc = unsafe {
                GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
            };
            if rc != ERROR_SUCCESS {
                bail!("Failed to snapshot privacy mode 2 display topology: {}", rc.0);
            }
            if path_count == 0 {
                bail!("No active display topology to snapshot.");
            }
            let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
            let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
            let rc = unsafe {
                QueryDisplayConfig(
                    QDC_ONLY_ACTIVE_PATHS,
                    &mut path_count,
                    paths.as_mut_ptr(),
                    &mut mode_count,
                    modes.as_mut_ptr(),
                    None,
                )
            };
            if rc == ERROR_INSUFFICIENT_BUFFER {
                continue;
            }
            if rc != ERROR_SUCCESS {
                bail!("Failed to snapshot privacy mode 2 display topology: {}", rc.0);
            }
            paths.truncate(path_count as usize);
            modes.truncate(mode_count as usize);
            log::debug!("Privacy mode 2 snapshot: paths={}, modes={}", paths.len(), modes.len());
            return Ok(Self { paths, modes });
        }
        bail!("Display topology changed while preparing privacy mode 2.");
    }

    pub(super) fn restore(&self) -> ResultType<()> {
        // Supplying only the original active paths also keeps originally disabled
        // physical screens disabled, rather than loading a saved extend topology.
        let rc = unsafe {
            SetDisplayConfig(
                Some(&self.paths),
                Some(&self.modes),
                SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_SAVE_TO_DATABASE,
            )
        };
        log::debug!("Privacy mode 2 snapshot restore: paths={}, result={}", self.paths.len(), rc);
        if rc != ERROR_SUCCESS.0 as i32 {
            bail!("Failed to restore privacy mode 2 display topology: {}", rc);
        }
        Ok(())
    }
}
