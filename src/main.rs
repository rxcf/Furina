use hudhook::inject::Process;

fn main() {
    let mut cur_dll = std::env::current_exe().unwrap();
    cur_dll.set_file_name("libfurina.dll");
    let cur_dll = cur_dll.canonicalize().unwrap();

    Process::by_name("PixelWorlds.exe").unwrap().inject(cur_dll).unwrap();
}
