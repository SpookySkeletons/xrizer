use log;
use openvr::{self as vr, HmdQuad_t, HmdVector2_t};
use serde::{Deserialize, Serialize};
use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Default, macros::InterfaceImpl)]
#[interface = "IVRChaperoneSetup"]
#[versions(006)]
pub struct ChaperoneSetup {
    vtables: Vtables,
    working_copy: Mutex<Option<ChaperoneInfo>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChaperoneInfo {
    jsonid: String,
    universes: Vec<Universe>,
    version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Universe {
    collision_bounds: Vec<Vec<[f32; 3]>>,
    play_area: [f32; 2],
    seated: Pose,
    standing: Pose,
    time: String,
    #[serde(rename = "universeID")]
    universe_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pose {
    translation: [f32; 3],
    yaw: f32,
}

impl ChaperoneSetup {
    fn get_config_path() -> PathBuf {
        // First try to find Steam config directory
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home".to_string());
        let steam_path = Path::new(&home).join(".steam/steam/config");

        if steam_path.exists() {
            steam_path
        } else {
            // Fallback to XDG_CONFIG_HOME
            let config_home =
                std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{}/.config", home));
            Path::new(&config_home).join("openvr")
        }
    }

    fn get_chaperone_path() -> PathBuf {
        Self::get_config_path().join("chaperone_info.vrchap")
    }

    fn get_lock_path() -> PathBuf {
        Self::get_config_path().join("chaperone_info.vrchap.lck")
    }

    fn get_active_universe_path() -> PathBuf {
        Self::get_config_path().join("lighthouse/active_universe.txt")
    }

    fn acquire_lock() -> Result<File, std::io::Error> {
        use std::os::unix::fs::OpenOptionsExt;

        OpenOptions::new()
            .write(true)
            .create(true)
            .custom_flags(libc::O_EXCL)
            .open(Self::get_lock_path())
    }

    fn release_lock(lock_file: File) {
        drop(lock_file);
        let _ = std::fs::remove_file(Self::get_lock_path());
    }

    fn read_active_universe() -> Option<String> {
        std::fs::read_to_string(Self::get_active_universe_path())
            .ok()
            .map(|s| s.trim().to_string())
    }

    fn load_chaperone_info() -> Result<ChaperoneInfo, Box<dyn std::error::Error>> {
        let path = Self::get_chaperone_path();

        if !path.exists() {
            // Create default chaperone info
            return Ok(ChaperoneInfo {
                jsonid: "chaperone_info".to_string(),
                universes: Vec::new(),
                version: 5,
            });
        }

        let mut file = File::open(&path)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;

        Ok(serde_json::from_str(&contents)?)
    }

    fn save_chaperone_info(info: &ChaperoneInfo) -> Result<(), Box<dyn std::error::Error>> {
        let path = Self::get_chaperone_path();

        // Create directory if it doesn't exist
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(info)?;
        let mut file = File::create(&path)?;
        file.write_all(json.as_bytes())?;

        Ok(())
    }

    fn create_default_universe(universe_id: String) -> Universe {
        Universe {
            collision_bounds: vec![
                vec![
                    [1.5, 0.0, 1.0],
                    [1.5, 5.0, 1.0],
                    [1.5, 5.0, -1.0],
                    [1.5, 0.0, -1.0],
                ],
                vec![
                    [1.5, 0.0, -1.0],
                    [1.5, 5.0, -1.0],
                    [-1.5, 5.0, -1.0],
                    [-1.5, 0.0, -1.0],
                ],
                vec![
                    [-1.5, 0.0, -1.0],
                    [-1.5, 5.0, -1.0],
                    [-1.5, 5.0, 1.0],
                    [-1.5, 0.0, 1.0],
                ],
                vec![
                    [-1.5, 0.0, 1.0],
                    [-1.5, 5.0, 1.0],
                    [1.5, 5.0, 1.0],
                    [1.5, 0.0, 1.0],
                ],
            ],
            play_area: [3.0, 2.0],
            seated: Pose {
                translation: [0.0, 0.0, 0.0],
                yaw: 0.0,
            },
            standing: Pose {
                translation: [0.0, 0.0, 0.0],
                yaw: 0.0,
            },
            time: time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc2822)
                .unwrap_or_else(|_| "Unknown".to_string()),
            universe_id,
        }
    }
}

impl vr::IVRChaperoneSetup006_Interface for ChaperoneSetup {
    fn CommitWorkingCopy(&self, _config_file: vr::EChaperoneConfigFile) -> bool {
        log::info!("CommitWorkingCopy called");

        let working_copy = self.working_copy.lock().unwrap();

        if let Some(info) = working_copy.as_ref() {
            // Acquire lock
            let lock_file = match Self::acquire_lock() {
                Ok(lock) => lock,
                Err(e) => {
                    log::error!("Failed to acquire lock: {}", e);
                    return false;
                }
            };

            // Save to disk
            if let Err(e) = Self::save_chaperone_info(info) {
                log::error!("Failed to save chaperone info: {}", e);
                Self::release_lock(lock_file);
                return false;
            }

            Self::release_lock(lock_file);
            log::info!("Successfully committed chaperone info");
            true
        } else {
            log::warn!("No working copy to commit");
            false
        }
    }

    fn RevertWorkingCopy(&self) {
        log::info!("RevertWorkingCopy called");
        let mut working_copy = self.working_copy.lock().unwrap();
        *working_copy = None;
    }

    fn GetWorkingPlayAreaSize(&self, size_x: *mut f32, size_z: *mut f32) -> bool {
        if !size_x.is_null() && !size_z.is_null() {
            let working_copy = self.working_copy.lock().unwrap();

            if let Some(info) = working_copy.as_ref() {
                // Get active universe
                if let Some(universe_id) = Self::read_active_universe() {
                    if let Some(universe) =
                        info.universes.iter().find(|u| u.universe_id == universe_id)
                    {
                        unsafe {
                            *size_x = universe.play_area[0];
                            *size_z = universe.play_area[1];
                        }
                        return true;
                    }
                }
            }

            // Default values
            unsafe {
                *size_x = 3.0;
                *size_z = 2.0;
            }
        }
        false
    }

    fn GetWorkingPlayAreaRect(&self, _: *mut vr::HmdQuad_t) -> bool {
        crate::warn_unimplemented!("GetWorkingPlayAreaRect");
        false
    }

    fn GetWorkingCollisionBoundsInfo(&self, _: *mut vr::HmdQuad_t, quads_count: *mut u32) -> bool {
        crate::warn_unimplemented!("GetWorkingCollisionBoundsInfo");
        if !quads_count.is_null() {
            unsafe {
                *quads_count = 0;
            }
        }
        false
    }

    fn GetLiveCollisionBoundsInfo(&self, _: *mut vr::HmdQuad_t, quads_count: *mut u32) -> bool {
        crate::warn_unimplemented!("GetLiveCollisionBoundsInfo");
        if !quads_count.is_null() {
            unsafe {
                *quads_count = 0;
            }
        }
        false
    }

    fn GetWorkingSeatedZeroPoseToRawTrackingPose(&self, _: *mut vr::HmdMatrix34_t) -> bool {
        crate::warn_unimplemented!("GetWorkingSeatedZeroPoseToRawTrackingPose");
        false
    }

    fn GetWorkingStandingZeroPoseToRawTrackingPose(&self, matrix: *mut vr::HmdMatrix34_t) -> bool {
        if matrix.is_null() {
            return false;
        }

        let working_copy = self.working_copy.lock().unwrap();

        if let Some(info) = working_copy.as_ref() {
            // Get active universe
            if let Some(universe_id) = Self::read_active_universe() {
                if let Some(universe) = info.universes.iter().find(|u| u.universe_id == universe_id)
                {
                    unsafe {
                        // Create identity matrix
                        (*matrix).m = [
                            [1.0, 0.0, 0.0, 0.0],
                            [0.0, 1.0, 0.0, 0.0],
                            [0.0, 0.0, 1.0, 0.0],
                        ];

                        // Set translation from standing pose
                        (*matrix).m[0][3] = universe.standing.translation[0];
                        (*matrix).m[1][3] = universe.standing.translation[1];
                        (*matrix).m[2][3] = universe.standing.translation[2];

                        // TODO: Apply yaw rotation if needed
                    }
                    return true;
                }
            }
        }

        // Return identity if no data
        unsafe {
            (*matrix).m = [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ];
        }
        false
    }

    fn SetWorkingPlayAreaSize(&self, size_x: f32, size_z: f32) {
        log::info!("SetWorkingPlayAreaSize: {}x{}", size_x, size_z);

        let mut working_copy = self.working_copy.lock().unwrap();

        // Ensure we have a working copy
        if working_copy.is_none() {
            match Self::load_chaperone_info() {
                Ok(info) => *working_copy = Some(info),
                Err(e) => {
                    log::error!("Failed to load chaperone info: {}", e);
                    return;
                }
            }
        }

        if let Some(info) = working_copy.as_mut() {
            if let Some(universe_id) = Self::read_active_universe() {
                // Find or create universe
                if let Some(universe) = info
                    .universes
                    .iter_mut()
                    .find(|u| u.universe_id == universe_id)
                {
                    universe.play_area = [size_x, size_z];
                } else {
                    // Create new universe
                    let mut new_universe = Self::create_default_universe(universe_id);
                    new_universe.play_area = [size_x, size_z];
                    info.universes.push(new_universe);
                }
            }
        }
    }

    fn SetWorkingCollisionBoundsInfo(&self, _: *mut HmdQuad_t, _: u32) {
        crate::warn_unimplemented!("SetWorkingCollisionBoundsInfo");
    }

    fn SetWorkingPerimeter(&self, _: *mut HmdVector2_t, _: u32) {
        crate::warn_unimplemented!("SetWorkingPerimeter");
    }

    fn SetWorkingSeatedZeroPoseToRawTrackingPose(&self, _: *const vr::HmdMatrix34_t) {
        crate::warn_unimplemented!("SetWorkingSeatedZeroPoseToRawTrackingPose");
    }

    fn SetWorkingStandingZeroPoseToRawTrackingPose(&self, matrix: *const vr::HmdMatrix34_t) {
        log::info!("SetWorkingStandingZeroPoseToRawTrackingPose called");

        if matrix.is_null() {
            log::error!("Null matrix provided");
            return;
        }

        let mut working_copy = self.working_copy.lock().unwrap();

        // Ensure we have a working copy
        if working_copy.is_none() {
            match Self::load_chaperone_info() {
                Ok(info) => *working_copy = Some(info),
                Err(e) => {
                    log::error!("Failed to load chaperone info: {}", e);
                    return;
                }
            }
        }

        if let Some(info) = working_copy.as_mut() {
            if let Some(universe_id) = Self::read_active_universe() {
                log::info!("Active universe: {}", universe_id);

                // Find or create universe
                let universe = if let Some(u) = info
                    .universes
                    .iter_mut()
                    .find(|u| u.universe_id == universe_id)
                {
                    u
                } else {
                    log::info!("Creating new universe entry for {}", universe_id);
                    info.universes
                        .push(Self::create_default_universe(universe_id.clone()));
                    info.universes.last_mut().unwrap()
                };

                unsafe {
                    // Extract translation from the matrix
                    // IMPORTANT: The matrix we receive is in STAGE SPACE (floor-relative)
                    // But the offset stored in vrchap needs to be in RAW TRACKING SPACE
                    let stage_x = (*matrix).m[0][3];
                    let stage_y = (*matrix).m[1][3];
                    let stage_z = (*matrix).m[2][3];

                    log::info!(
                        "Stage space matrix translation: [{}, {}, {}]",
                        stage_x,
                        stage_y,
                        stage_z
                    );

                    // Get current raw space offset
                    let current_raw_offset_y = universe.standing.translation[1];
                    log::info!("Current raw offset Y: {}", current_raw_offset_y);

                    // When vrcmd --resetroomsetup is called with the HMD at floor level:
                    // - HMD is at Y=0 in stage space (floor level)
                    // - But in raw space, HMD might be at Y=2.5m
                    // - So we need raw_offset = 2.5m to bring floor to raw position

                    // The stage_y value tells us how much to adjust from current floor
                    // If stage_y = -1.7, that means HMD is 1.7m below current floor
                    // So new floor should be 1.7m lower in raw space too
                    let new_raw_offset_y = current_raw_offset_y - stage_y;

                    log::info!(
                        "New raw offset Y: {} (was {} - {})",
                        new_raw_offset_y,
                        current_raw_offset_y,
                        stage_y
                    );

                    universe.standing.translation[0] = universe.standing.translation[0] - stage_x;
                    universe.standing.translation[1] = new_raw_offset_y;
                    universe.standing.translation[2] = universe.standing.translation[2] - stage_z;

                    // TODO: Extract yaw from matrix if needed

                    // Update seated to match standing for now
                    universe.seated = universe.standing.clone();

                    // Update timestamp
                    universe.time = time::OffsetDateTime::now_utc()
                        .format(&time::format_description::well_known::Rfc2822)
                        .unwrap_or_else(|_| "Unknown".to_string());
                }

                log::info!("Updated standing pose: {:?}", universe.standing);
            } else {
                log::error!("No active universe found");
            }
        }
    }

    fn ReloadFromDisk(&self, _config_file: vr::EChaperoneConfigFile) {
        log::info!("ReloadFromDisk called");

        let mut working_copy = self.working_copy.lock().unwrap();

        match Self::load_chaperone_info() {
            Ok(info) => {
                *working_copy = Some(info);
                log::info!("Successfully reloaded chaperone info from disk");
            }
            Err(e) => {
                log::error!("Failed to reload chaperone info: {}", e);
                *working_copy = None;
            }
        }
    }

    fn GetLiveSeatedZeroPoseToRawTrackingPose(&self, _: *mut vr::HmdMatrix34_t) -> bool {
        crate::warn_unimplemented!("GetLiveSeatedZeroPoseToRawTrackingPose");
        false
    }

    fn ExportLiveToBuffer(&self, _: *mut c_char, buffer_length: *mut u32) -> bool {
        crate::warn_unimplemented!("ExportLiveToBuffer");
        if !buffer_length.is_null() {
            unsafe {
                *buffer_length = 0;
            }
        }
        false
    }

    fn ImportFromBufferToWorking(&self, buffer: *const c_char, _: u32) -> bool {
        let _buffer_str = if buffer.is_null() {
            "null".to_string()
        } else {
            unsafe { CStr::from_ptr(buffer) }
                .to_string_lossy()
                .to_string()
        };
        crate::warn_unimplemented!("ImportFromBufferToWorking");
        false
    }

    fn ShowWorkingSetPreview(&self) {
        crate::warn_unimplemented!("ShowWorkingSetPreview");
    }

    fn HideWorkingSetPreview(&self) {
        crate::warn_unimplemented!("HideWorkingSetPreview");
    }

    fn RoomSetupStarting(&self) {
        crate::warn_unimplemented!("RoomSetupStarting");
    }
}
