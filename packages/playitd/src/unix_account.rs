use uzers::os::unix::GroupExt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnixUserAccount {
    pub username: String,
    pub primary_gid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnixGroupInfo {
    pub name: String,
    pub gid: u32,
    pub members: Vec<String>,
}

pub fn effective_uid() -> u32 {
    uzers::get_effective_uid() as u32
}

pub fn effective_gid() -> u32 {
    uzers::get_effective_gid() as u32
}

pub fn current_user_is_root() -> bool {
    effective_uid() == 0
}

pub fn group_gid_by_name(name: &str) -> Option<u32> {
    uzers::get_group_by_name(name).map(|group| group.gid() as u32)
}

pub fn group_info_by_gid(gid: u32) -> Option<UnixGroupInfo> {
    let group = uzers::get_group_by_gid(gid as uzers::gid_t)?;
    Some(UnixGroupInfo {
        name: group.name().to_string_lossy().into_owned(),
        gid: group.gid() as u32,
        members: group
            .members()
            .iter()
            .map(|member| member.to_string_lossy().into_owned())
            .collect(),
    })
}

pub fn current_user_account() -> Option<UnixUserAccount> {
    let user = uzers::get_user_by_uid(effective_uid() as uzers::uid_t)?;
    Some(UnixUserAccount {
        username: user.name().to_string_lossy().into_owned(),
        primary_gid: user.primary_group_id() as u32,
    })
}

pub fn current_process_has_group(gid: u32) -> bool {
    if effective_gid() == gid {
        return true;
    }

    uzers::group_access_list()
        .map(|groups| groups.into_iter().any(|group| group.gid() as u32 == gid))
        .unwrap_or(false)
}

pub fn username_by_uid(uid: u32) -> Option<String> {
    uzers::get_user_by_uid(uid as uzers::uid_t)
        .map(|user| user.name().to_string_lossy().into_owned())
}

/// Whether the account database puts `uid` in group `gid`, as its primary group or a
/// supplementary one. This reads the database on every call, so it sees `usermod -aG` right
/// away. A process's own groups only change when its user logs in again.
pub fn user_is_in_group(uid: u32, gid: u32) -> bool {
    let Some(user) = uzers::get_user_by_uid(uid as uzers::uid_t) else {
        return false;
    };
    let primary_gid = user.primary_group_id();
    if primary_gid as u32 == gid {
        return true;
    }
    uzers::get_user_groups(user.name(), primary_gid)
        .is_some_and(|groups| groups.iter().any(|group| group.gid() as u32 == gid))
}
