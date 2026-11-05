//! 外部子进程的统一创建入口。
//!
//! `ffmpeg.exe` / `ffprobe.exe` / `powershell.exe` 都是控制台子系统程序。
//! `std::process::Command` 在 Windows 上的进程创建标志默认为 0，启动这类程序时
//! 系统会分配（或前台激活）一个可见的 conhost 黑色窗口：GUI 应用每调用一次就
//! 闪一次，长驻的预览解码进程更会让窗口一直挂在桌面上。
//!
//! 这里统一附加 `CREATE_NO_WINDOW` (0x08000000)。该标志只抑制控制台窗口，
//! `stdin` / `stdout` / `stderr` 的管道重定向与退出码语义完全不变，因此可以
//! 直接替换所有 `Command::new(...)` 构造点。
//!
//! 约束：不能与 `DETACHED_PROCESS` / `CREATE_NEW_CONSOLE` 同时使用；对本身是
//! GUI 子系统的子程序无影响。非 Windows 平台等价于 `Command::new(program)`。

use std::process::Command;

/// 构造一个不会弹出控制台窗口的子进程命令。
pub fn hidden_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

#[cfg(test)]
mod tests {
    use super::hidden_command;

    /// 验证 helper 构造的命令可以正常 spawn 并回收输出。
    /// 用当前测试可执行文件作为无害目标，不依赖系统是否安装 ffmpeg。
    #[test]
    fn hidden_command_spawns_and_captures_output() {
        let program = std::env::current_exe().expect("current_exe");
        let output = hidden_command(&program)
            .arg("--help")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output();
        assert!(output.is_ok(), "hidden_command 应能启动子进程: {output:?}");
    }
}
