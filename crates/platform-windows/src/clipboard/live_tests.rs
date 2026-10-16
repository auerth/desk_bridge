//! Integration tests against the REAL system clipboard. They overwrite the user's clipboard, so
//! every test is `#[ignore]`. Run manually and serially:
//!
//! `cargo test -p deskbridge-platform-windows -- --ignored --test-threads=1 clipboard::live`

use super::format::{self, CF_UNICODETEXT};
use super::*;
use deskbridge_platform::clipboard::{ClipboardBackend, ClipboardEvent, ClipboardProvider, VirtualFileProvider};
use deskbridge_platform::{UnboundedReceiver, unbounded_channel};
use std::io::{Cursor, Read};
use std::process::Command;
use std::sync::atomic::AtomicUsize;
use std::time::Instant;
use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL, TYMED_ISTREAM};
use windows::Win32::System::Ole::{OleGetClipboard, OleInitialize, OleUninitialize};

fn start() -> (WinClipboard, UnboundedReceiver<ClipboardEvent>) {
    let cb = WinClipboard::new();
    let (tx, rx) = unbounded_channel();
    cb.start(tx).expect("start");
    (cb, rx)
}

fn ps(script: &str, sta: bool) -> String {
    let mut c = Command::new("powershell.exe");
    c.args(["-NoProfile", "-NonInteractive"]);
    if sta {
        c.arg("-STA");
    }
    let out = c.args(["-Command", &format!("[Console]::OutputEncoding=[Text.Encoding]::UTF8; {script}")]).output().expect("powershell");
    assert!(out.status.success(), "powershell failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim_end().to_owned()
}

fn wait_event(rx: &mut UnboundedReceiver<ClipboardEvent>, secs: u64) -> Option<ClipboardEvent> {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if let Ok(e) = rx.try_recv() {
            return Some(e);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    None
}

fn assert_no_event(rx: &mut UnboundedReceiver<ClipboardEvent>) {
    std::thread::sleep(Duration::from_millis(700));
    if let Ok(e) = rx.try_recv() {
        panic!("unexpected echo event: {e:?}");
    }
}

#[test]
#[ignore = "overwrites the real clipboard"]
fn live_text_roundtrip_no_echo_and_external_change() {
    let (cb, mut rx) = start();
    cb.set_text("Grüße 😀 text").unwrap();
    assert_eq!(cb.read(mime::TEXT).unwrap().unwrap(), "Grüße 😀 text".as_bytes());
    assert!(cb.read(mime::PNG).unwrap().is_none());
    assert_no_event(&mut rx); // own write is not echoed
    assert_eq!(ps("Get-Clipboard -Raw", false), "Grüße 😀 text");

    // External change -> event with eager text.
    ps("Set-Clipboard -Value ('ext' + [char]0xE4 + [char]0xF6)", false);
    let Some(ClipboardEvent::Changed { formats, sensitive, text, files }) = wait_event(&mut rx, 5) else { panic!("no event for external change") };
    assert!(!sensitive);
    assert_eq!(text.as_deref(), Some("extäö"));
    assert!(files.is_empty());
    assert_eq!(formats[0].mime, mime::TEXT);
    assert_eq!(formats[0].size, "extäö".len() as u64);
    cb.stop();
}

#[test]
#[ignore = "overwrites the real clipboard"]
fn live_sensitive_flags_from_other_thread() {
    let (cb, mut rx) = start();
    let exclude = register_format(format::NAME_EXCLUDE_MONITOR);
    let can_hist = register_format(format::NAME_CAN_INCLUDE_HISTORY);
    let write = |extra: Option<(u32, Vec<u8>)>| {
        let t = std::thread::spawn(move || unsafe {
            OpenClipboard(HWND::default()).unwrap();
            EmptyClipboard().unwrap();
            set_clipboard_bytes(CF_UNICODETEXT, &format::string_to_utf16_bytes("s3cret")).unwrap();
            if let Some((f, b)) = extra {
                set_clipboard_bytes(f, &b).unwrap();
            }
            CloseClipboard().unwrap();
        });
        t.join().unwrap();
    };
    write(Some((exclude, vec![1, 0, 0, 0])));
    let Some(ClipboardEvent::Changed { sensitive, text, .. }) = wait_event(&mut rx, 5) else { panic!("no event") };
    assert!(sensitive);
    assert_eq!(text, None, "sensitive text must not be read");
    write(Some((can_hist, vec![0, 0, 0, 0])));
    let Some(ClipboardEvent::Changed { sensitive, .. }) = wait_event(&mut rx, 5) else { panic!("no event") };
    assert!(sensitive);
    write(Some((can_hist, vec![1, 0, 0, 0])));
    let Some(ClipboardEvent::Changed { sensitive, text, .. }) = wait_event(&mut rx, 5) else { panic!("no event") };
    assert!(!sensitive);
    assert_eq!(text.as_deref(), Some("s3cret"));
    cb.stop();
}

struct TestProvider {
    fetches: AtomicUsize,
    html: String,
    png: Vec<u8>,
}

impl ClipboardProvider for TestProvider {
    fn fetch(&self, m: &str) -> deskbridge_platform::Result<Vec<u8>> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        match m {
            mime::TEXT => Ok("delayed ünïcode text".as_bytes().to_vec()),
            mime::HTML => Ok(self.html.clone().into_bytes()),
            mime::RTF => Ok(b"{\\rtf1\\ansi delayed rtf}".to_vec()),
            mime::PNG => Ok(self.png.clone()),
            _ => Err(PlatformError::other("nope")),
        }
    }
}

fn info(m: &str) -> ClipFormatInfo {
    ClipFormatInfo { mime: m.into(), size: 0 }
}

#[test]
#[ignore = "overwrites the real clipboard"]
fn live_delayed_rendering_text_html_rtf_png() {
    let (cb, mut rx) = start();
    let img = image::Rgba { width: 5, height: 3, data: (0..60u8).map(|i| if i % 4 == 3 { 200 } else { i * 4 }).collect() };
    let provider = Arc::new(TestProvider {
        fetches: AtomicUsize::new(0),
        html: "<b>Grüße 日本</b>".into(),
        png: image::encode_png(&img).unwrap(),
    });
    cb.offer(vec![info(mime::TEXT), info(mime::HTML), info(mime::RTF), info(mime::PNG)], provider.clone()).unwrap();
    assert_no_event(&mut rx);
    assert_eq!(provider.fetches.load(Ordering::SeqCst), 0, "nothing fetched before a paste");

    // Paste text from another process.
    assert_eq!(ps("Get-Clipboard -Raw", false), "delayed ünïcode text");
    assert_eq!(provider.fetches.load(Ordering::SeqCst), 1);

    // HTML (CF_HTML wrapper) as seen by a .NET consumer.
    let html = ps("Add-Type -AssemblyName System.Windows.Forms; [Windows.Forms.Clipboard]::GetText([Windows.Forms.TextDataFormat]::Html)", true);
    assert!(html.starts_with("Version:0.9"), "{html}");
    assert!(html.contains("<b>Grüße 日本</b>") || html.contains("Gr"), "{html}");

    // PNG / DIBV5 as .NET bitmap (picks CF_BITMAP/DIB synthesised from our DIBV5 or PNG).
    let dims = ps("Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing; $i=[Windows.Forms.Clipboard]::GetImage(); '{0}x{1}' -f $i.Width,$i.Height", true);
    assert_eq!(dims, "5x3");
    let png_ok = ps(
        "Add-Type -AssemblyName System.Windows.Forms; $d=[Windows.Forms.Clipboard]::GetDataObject(); if($d.GetDataPresent('PNG')){'png'}else{'nopng'}; if($d.GetDataPresent('RTF')){'rtf'}",
        true,
    );
    assert!(png_ok.contains("png") && !png_ok.contains("nopng"), "{png_ok}");

    // Reading our own delayed content through the backend (WM_RENDERFORMAT to ourselves).
    assert_eq!(cb.read(mime::TEXT).unwrap().unwrap(), "delayed ünïcode text".as_bytes());
    let html = String::from_utf8(cb.read(mime::HTML).unwrap().unwrap()).unwrap();
    assert!(html.contains("<b>Grüße 日本</b>") && html.contains("StartFragment"), "{html}");
    let png = cb.read(mime::PNG).unwrap().unwrap();
    assert_eq!(image::decode_png(&png).unwrap(), img);
    assert_eq!(cb.read(mime::RTF).unwrap().unwrap(), b"{\\rtf1\\ansi delayed rtf}");
    assert_no_event(&mut rx);

    // Replacing the offer: the old provider is released, new content works.
    cb.set_text("plain").unwrap();
    assert_eq!(ps("Get-Clipboard -Raw", false), "plain");
    cb.stop();
}

#[test]
#[ignore = "overwrites the real clipboard"]
fn live_failing_provider_does_not_hang() {
    struct Failing;
    impl ClipboardProvider for Failing {
        fn fetch(&self, _m: &str) -> deskbridge_platform::Result<Vec<u8>> {
            Err(PlatformError::other("peer gone"))
        }
    }
    let (cb, _rx) = start();
    cb.offer(vec![info(mime::TEXT)], Arc::new(Failing)).unwrap();
    let t = Instant::now();
    let out = ps("Get-Clipboard -Raw", false);
    assert!(t.elapsed() < Duration::from_secs(15));
    assert!(out.is_empty(), "{out:?}");
    cb.stop();
}

#[test]
#[ignore = "overwrites the real clipboard"]
fn live_hdrop_local_files_and_external_files_event() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a ü.txt");
    let b = dir.path().join("b.txt");
    std::fs::write(&a, b"A").unwrap();
    std::fs::write(&b, b"B").unwrap();
    let (cb, mut rx) = start();
    cb.offer_local_files(vec![a.clone(), b.clone()]).unwrap();
    assert_no_event(&mut rx);
    let names = ps("(Get-Clipboard -Format FileDropList | ForEach-Object { $_.Name }) -join '|'", false);
    assert_eq!(names, "a ü.txt|b.txt");
    let effect = ps(
        "Add-Type -AssemblyName System.Windows.Forms; $m=[Windows.Forms.Clipboard]::GetData('Preferred DropEffect'); $b=New-Object byte[] 4; [void]$m.Read($b,0,4); [BitConverter]::ToInt32($b,0)",
        true,
    );
    assert_eq!(effect, "1");
    let uris = String::from_utf8(cb.read(mime::URI_LIST).unwrap().unwrap()).unwrap();
    assert!(uris.starts_with("file:///") && uris.contains("a%20%C3%BC.txt"), "{uris}");

    // External file copy -> event with the paths.
    ps(&format!("Set-Clipboard -Path '{}'", b.display()), false);
    let Some(ClipboardEvent::Changed { formats, files, .. }) = wait_event(&mut rx, 5) else { panic!("no event") };
    assert_eq!(files, vec![b.clone()]);
    assert!(formats.iter().any(|f| f.mime == mime::URI_LIST));
    cb.stop();
}

// ------------------------------------------------------------------ virtual files

struct FileProvider {
    files: Vec<Vec<u8>>,
    opens: AtomicUsize,
}

impl VirtualFileProvider for FileProvider {
    fn open(&self, index: usize) -> deskbridge_platform::Result<Box<dyn Read + Send>> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Cursor::new(self.files[index].clone())))
    }
}

fn offers() -> (Vec<FileOffer>, Vec<Vec<u8>>) {
    let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let datas = vec![Vec::new(), b"hello virtual".to_vec(), big, Vec::new()];
    let mk = |p: &str, d: &Vec<u8>, dir: bool| FileOffer { rel_path: p.into(), size: d.len() as u64, mtime_unix_ms: 1_700_000_000_000, is_dir: dir };
    let offers = vec![mk("Folder", &datas[0], true), mk("Folder/hello ü.txt", &datas[1], false), mk("Folder/big.bin", &datas[2], false), mk("empty.txt", &datas[3], false)];
    (offers, datas)
}

/// Reads the clipboard data object like Explorer would, but in-process on another thread.
fn read_virtual_files_in_process() -> Vec<(String, Vec<u8>)> {
    std::thread::spawn(|| unsafe {
        OleInitialize(None).unwrap();
        let obj: IDataObject = OleGetClipboard().unwrap();
        let desc_fmt = register_format(format::NAME_FILE_DESCRIPTOR) as u16;
        let cont_fmt = register_format(format::NAME_FILE_CONTENTS) as u16;
        let fe = FORMATETC { cfFormat: desc_fmt, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
        assert!(obj.QueryGetData(&fe).is_ok());
        let med = obj.GetData(&fe).unwrap();
        let h = med.u.hGlobal;
        let size = windows::Win32::System::Memory::GlobalSize(h);
        let p = GlobalLock(h).cast::<u8>();
        let desc = std::slice::from_raw_parts(p, size).to_vec();
        let _ = GlobalUnlock(h);
        let count = u32::from_le_bytes(desc[0..4].try_into().unwrap()) as usize;
        let mut out = Vec::new();
        for i in 0..count {
            let e = 4 + i * 592;
            let attrs = u32::from_le_bytes(desc[e + 36..e + 40].try_into().unwrap());
            let name = format::utf16_bytes_to_string(&desc[e + 72..e + 72 + 520]);
            if attrs & 0x10 != 0 {
                out.push((name, Vec::new()));
                continue;
            }
            let fe = FORMATETC { cfFormat: cont_fmt, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: i as i32, tymed: TYMED_ISTREAM.0 as u32 };
            let med = obj.GetData(&fe).unwrap();
            let stream = (*med.u.pstm).clone().expect("stream");
            let mut data = Vec::new();
            let mut buf = vec![0u8; 100_000];
            loop {
                let mut n = 0u32;
                let hr = stream.Read(buf.as_mut_ptr().cast(), buf.len() as u32, Some(&mut n));
                assert!(hr.is_ok(), "Read failed: {hr:?}");
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&buf[..n as usize]);
            }
            // Stat reports the size.
            let mut st = windows::Win32::System::Com::STATSTG::default();
            stream.Stat(&mut st, windows::Win32::System::Com::STATFLAG_NONAME).unwrap();
            assert_eq!(st.cbSize as usize, data.len());
            out.push((name, data));
        }
        drop(obj);
        OleUninitialize();
        out
    })
    .join()
    .unwrap()
}

#[test]
#[ignore = "overwrites the real clipboard"]
fn live_virtual_files_data_object_in_process() {
    let (cb, mut rx) = start();
    let (offers, datas) = offers();
    let provider = Arc::new(FileProvider { files: datas.clone(), opens: AtomicUsize::new(0) });
    cb.offer_files(offers, provider.clone()).unwrap();
    assert_no_event(&mut rx);
    let got = read_virtual_files_in_process();
    assert_eq!(got.len(), 4);
    assert_eq!(got[0].0, "Folder");
    assert_eq!(got[1], ("Folder\\hello ü.txt".to_string(), datas[1].clone()));
    assert_eq!(got[2].0, "Folder\\big.bin");
    assert_eq!(got[2].1, datas[2]);
    assert_eq!(got[3], ("empty.txt".to_string(), Vec::new()));
    assert_eq!(provider.opens.load(Ordering::SeqCst), 2, "empty files are never opened");
    assert_no_event(&mut rx);
    // A foreign copy replaces the virtual files and is reported as an event.
    ps("Set-Clipboard -Value 'after'", false);
    assert!(matches!(wait_event(&mut rx, 5), Some(ClipboardEvent::Changed { .. })));
    cb.stop();
}

/// Out-of-process consumer: a tiny C# `OleGetClipboard` client (what Explorer does), run in STA
/// PowerShell. Copies every virtual file into `out_dir`.
const PS_PASTE: &str = r#"
Add-Type -TypeDefinition @"
using System; using System.IO; using System.Runtime.InteropServices; using System.Runtime.InteropServices.ComTypes;
public static class VPaste {
  [DllImport("ole32.dll")] static extern int OleGetClipboard(out System.Runtime.InteropServices.ComTypes.IDataObject o);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern uint RegisterClipboardFormat(string n);
  [DllImport("kernel32.dll")] static extern IntPtr GlobalLock(IntPtr h);
  [DllImport("kernel32.dll")] static extern bool GlobalUnlock(IntPtr h);
  public static string Run(string outDir) {
    System.Runtime.InteropServices.ComTypes.IDataObject o; Marshal.ThrowExceptionForHR(OleGetClipboard(out o));
    short fd = (short)RegisterClipboardFormat("FileGroupDescriptorW"), fc = (short)RegisterClipboardFormat("FileContents");
    FORMATETC f = new FORMATETC { cfFormat = fd, dwAspect = DVASPECT.DVASPECT_CONTENT, lindex = -1, tymed = TYMED.TYMED_HGLOBAL };
    string dump = "";
    IEnumFORMATETC en = o.EnumFormatEtc(DATADIR.DATADIR_GET); FORMATETC[] a = new FORMATETC[1]; int[] fetched = new int[1];
    while (en.Next(1, a, fetched) == 0 && fetched[0] == 1) dump += a[0].cfFormat + "/" + a[0].lindex + "/" + a[0].tymed + " ";
    Console.WriteLine("formats: " + dump + " want fd=" + fd + " fc=" + fc);
    STGMEDIUM m; o.GetData(ref f, out m);
    IntPtr p = GlobalLock(m.unionmember);
    int n = Marshal.ReadInt32(p); string log = "";
    for (int i = 0; i < n; i++) {
      IntPtr e = p + 4 + i * 592;
      uint attrs = (uint)Marshal.ReadInt32(e, 36);
      string name = Marshal.PtrToStringUni(e + 72);
      string dst = Path.Combine(outDir, name);
      if ((attrs & 0x10) != 0) { Directory.CreateDirectory(dst); log += name + "/;"; continue; }
      Directory.CreateDirectory(Path.GetDirectoryName(dst));
      FORMATETC c = new FORMATETC { cfFormat = fc, dwAspect = DVASPECT.DVASPECT_CONTENT, lindex = i, tymed = TYMED.TYMED_ISTREAM };
      foreach (int li in new int[]{-1,0,1,2}) foreach (TYMED ty in new TYMED[]{TYMED.TYMED_ISTREAM, TYMED.TYMED_ISTREAM|TYMED.TYMED_HGLOBAL}) { FORMATETC q = new FORMATETC { cfFormat = fc, dwAspect = DVASPECT.DVASPECT_CONTENT, lindex = li, tymed = ty }; Console.WriteLine("Q li=" + li + " ty=" + ty + " -> " + o.QueryGetData(ref q)); }
      STGMEDIUM cm;
      try { o.GetData(ref c, out cm); } catch (Exception ex) { Console.WriteLine("GetData ex: " + ex.Message); throw; }
      IStream s = (IStream)Marshal.GetObjectForIUnknown(cm.unionmember);
      using (FileStream fs = File.Create(dst)) {
        byte[] buf = new byte[65536]; IntPtr pn = Marshal.AllocHGlobal(4);
        while (true) { s.Read(buf, buf.Length, pn); int r = Marshal.ReadInt32(pn); if (r == 0) break; fs.Write(buf, 0, r); }
        Marshal.FreeHGlobal(pn);
      }
      log += name + ";";
    }
    GlobalUnlock(m.unionmember);
    return log;
  }
}
"@
[VPaste]::Run($args[0])
"#;

#[test]
#[ignore = "overwrites the real clipboard"]
fn live_virtual_files_pasted_by_other_process() {
    let (cb, _rx) = start();
    let (offers, datas) = offers();
    let provider = Arc::new(FileProvider { files: datas.clone(), opens: AtomicUsize::new(0) });
    cb.offer_files(offers, provider).unwrap();
    let out = tempfile::tempdir().unwrap();
    let script_dir = tempfile::tempdir().unwrap();
    let script = script_dir.path().join("paste.ps1");
    std::fs::write(&script, PS_PASTE).unwrap();
    let o = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-STA", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .arg(out.path())
        .output()
        .unwrap();
    assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    let stdout = String::from_utf8_lossy(&o.stdout).into_owned();
    let listing: Vec<_> = walk(out.path());
    assert_eq!(std::fs::read(out.path().join("Folder").join("hello ü.txt")).unwrap_or_default(), datas[1], "stdout={stdout:?} stderr={:?} files={listing:?}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(std::fs::read(out.path().join("Folder").join("big.bin")).unwrap(), datas[2]);
    assert!(std::fs::read(out.path().join("empty.txt")).unwrap().is_empty());
    cb.stop();
}

#[test]
#[ignore = "overwrites the real clipboard"]
fn live_stop_clears_delayed_offer_and_restart_works() {
    let (cb, _rx) = start();
    let provider = Arc::new(TestProvider { fetches: AtomicUsize::new(0), html: String::new(), png: Vec::new() });
    cb.offer(vec![info(mime::TEXT)], provider.clone()).unwrap();
    cb.stop();
    assert!(cb.read(mime::TEXT).is_err(), "calls after stop fail cleanly");
    assert_eq!(ps("Get-Clipboard -Raw", false), "");
    assert_eq!(provider.fetches.load(Ordering::SeqCst), 0);
    // Immediate text survives a stop.
    let (tx, _rx2) = unbounded_channel();
    cb.start(tx).unwrap();
    cb.set_text("survivor").unwrap();
    cb.stop();
    assert_eq!(ps("Get-Clipboard -Raw", false), "survivor");
}

fn walk(p: &std::path::Path) -> Vec<String> {
    let mut v = Vec::new();
    for e in std::fs::read_dir(p).into_iter().flatten().flatten() {
        v.push(e.path().display().to_string());
        if e.path().is_dir() {
            v.extend(walk(&e.path()));
        }
    }
    v
}

#[test]
#[ignore = "overwrites the real clipboard"]
fn live_virtual_files_explorer_paste_verb() {
    let (cb, _rx) = start();
    let (offers, datas) = offers();
    let provider = Arc::new(FileProvider { files: datas.clone(), opens: AtomicUsize::new(0) });
    cb.offer_files(offers, provider.clone()).unwrap();
    let out = tempfile::tempdir().unwrap();
    let script = format!(
        "$sh = New-Object -ComObject Shell.Application; $ns = $sh.Namespace('{}'); $ns.Self.InvokeVerb('paste'); Start-Sleep -Seconds 4",
        out.path().display()
    );
    let _ = ps(&script, true);
    let listing = walk(out.path());
    assert_eq!(std::fs::read(out.path().join("Folder").join("hello ü.txt")).unwrap_or_default(), datas[1], "files={listing:?} opens={}", provider.opens.load(Ordering::SeqCst));
    assert_eq!(std::fs::read(out.path().join("Folder").join("big.bin")).unwrap(), datas[2]);
    cb.stop();
}

#[test]
#[ignore = "overwrites the real clipboard"]
fn live_dump_formats_after_ole() {
    let (cb, _rx) = start();
    let (offers, datas) = offers();
    cb.offer_files(offers, Arc::new(FileProvider { files: datas, opens: AtomicUsize::new(0) })).unwrap();
    let names = std::thread::spawn(|| unsafe {
        OpenClipboard(HWND::default()).unwrap();
        let mut v = Vec::new();
        let mut f = 0;
        loop {
            f = EnumClipboardFormats(f);
            if f == 0 {
                break;
            }
            v.push(format!("{f}:{}", registered_name(f).unwrap_or_default()));
        }
        let do_fmt = register_format("DataObject");
        let b = clipboard_bytes(do_fmt);
        v.push(format!("DataObject bytes={:?}", b.as_ref().map(Vec::len)));
        if let Some(b) = b {
            v.push(format!("{:02x?}", &b[..b.len().min(64)]));
        }
        CloseClipboard().unwrap();
        v
    })
    .join()
    .unwrap();
    eprintln!("FORMATS {names:?}");
    cb.stop();
}
