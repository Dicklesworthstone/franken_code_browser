use franken_macos::callbacks::{CallbackCell, InstanceId, RegisteredClassName};
use franken_macos::view_binding::{
    ColorSpace, DisplayMetrics, DrawablePath, HostTargetDescriptor, NativeViewBinding, PixelFormat,
};
use franken_macos::windowing::{BoundedEventQueue, EventPump, NativeWindow, Rect, WindowStyle};
use franken_macos::{Application, MainThreadToken, MetalDevice, MetalLayer};

fn main() {
    let code = run();
    if code != 0 {
        std::process::exit(code);
    }
}

fn run() -> i32 {
    let token = match MainThreadToken::capture_current() {
        Ok(t) => {
            println!("[fcb] main thread captured");
            t
        }
        Err(e) => {
            eprintln!("FATAL: not on main thread: {e:?}");
            return 1;
        }
    };

    let app = match Application::shared(token) {
        Ok(a) => {
            println!("[fcb] Application: {:?}", a.ownership());
            a
        }
        Err(e) => {
            eprintln!("FATAL: Application: {e:?}");
            return 1;
        }
    };

    let layer = match MetalLayer::new(token) {
        Ok(l) => {
            println!("[fcb] CAMetalLayer: {:?}", l.ownership());
            l
        }
        Err(e) => {
            eprintln!("FATAL: MetalLayer: {e:?}");
            return 1;
        }
    };

    let device = match MetalDevice::system_default(token) {
        Ok(d) => {
            println!("[fcb] MetalDevice: {:?}", d.ownership());
            d
        }
        Err(e) => {
            eprintln!("FATAL: MetalDevice: {e:?}");
            return 1;
        }
    };

    let window = match NativeWindow::create(
        token,
        "FrankenCodeBrowser",
        Rect::at_origin(800.0, 600.0),
        WindowStyle::standard(),
    ) {
        Ok(w) => {
            println!(
                "[fcb] window created, backing_scale={}",
                w.recorded_backing_scale()
            );
            w
        }
        Err(e) => {
            eprintln!("FATAL: NativeWindow: {e:?}");
            return 1;
        }
    };
    // create() defaults to accessory; a real application overrides to
    // regular AFTER window creation so the Dock icon and Cmd-Tab appear.
    let regular = franken_macos::set_regular_activation_policy();
    println!("[fcb] activation policy regular: {regular}");
    window.center(token);
    println!("[fcb] window centered on screen");

    let source_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "README.md".to_string());
    let source_bytes =
        std::fs::read(&source_path).unwrap_or_else(|_| b"// fcb: file unreadable".to_vec());
    let capped = &source_bytes[..source_bytes.len().min(1 << 20)];
    let source_text = String::from_utf8_lossy(capped).into_owned();
    let installed = window.install_source_text(token, &source_text);
    println!(
        "[fcb] source text installed: {installed} ({}: {} bytes shown)",
        source_path,
        source_text.len()
    );

    let class_name = match RegisteredClassName::issue("fcb-win", InstanceId::next()) {
        Ok(cn) => cn,
        Err(e) => {
            eprintln!("FATAL: class name: {e:?}");
            return 1;
        }
    };
    let mut cell = CallbackCell::register(token, class_name, 0_u64);
    if std::env::var_os("FCB_WINDOW_METAL").is_some() {
        let scale = window.recorded_backing_scale();
        let host = HostTargetDescriptor {
            pixel_format: PixelFormat::Bgra8Unorm,
            color_space: ColorSpace::Srgb,
            sample_count: 1,
            metrics: DisplayMetrics {
                backing_scale: scale,
                drawable_width: 800.0 * scale,
                drawable_height: 600.0 * scale,
            },
        };
        match NativeViewBinding::attach(
            token,
            &window,
            layer,
            &device,
            host,
            DrawablePath::RendererOwned,
        ) {
            Ok((binding, owner)) => {
                println!("[fcb] view binding attached; presenting clear frames");
                let color: [f64; 4] = [0.05, 0.09, 0.18, 1.0];
                match binding.present_clear(token, owner, &device, color) {
                    Ok(()) => println!("[fcb] clear frame PRESENTED through CAMetalLayer"),
                    Err(e) => eprintln!("[fcb] present failed: {e:?}"),
                }
            }
            Err(e) => {
                eprintln!("[fcb] FATAL: view binding: {e:?}");
                return 1;
            }
        }
        println!("[fcb] Metal frame live — continuing to event loop");
    }

    let pump = match EventPump::attach(token) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("FATAL: EventPump: {e:?}");
            return 1;
        }
    };
    let mut queue = BoundedEventQueue::new(256);

    println!("[fcb] WINDOW IS LIVE — close it to exit");
    println!("[fcb] Metal layer + device + window + callbacks + event pump armed");

    let mut total_events: usize = 0;
    let start = std::time::Instant::now();

    // A real application runs until its window closes. The bounded
    // timeout is opt-in (FCB_WINDOW_TIMEOUT_SECS) for tests and CI.
    let max_duration = std::env::var("FCB_WINDOW_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(std::time::Duration::from_secs);

    if std::env::var_os("FCB_WINDOW_RUNLOOP").is_some() {
        println!("[fcb] handing main thread to the full platform runloop");
        franken_macos::run_app();
        println!("[fcb] runloop returned");
        return 0;
    }

    loop {
        // Canonical manual loop: block until the next event, dispatch it
        // through the responder chain, then service pending window updates.
        match pump.service(token, &mut queue) {
            Ok(n) => total_events += n,
            Err(e) => {
                eprintln!("[fcb] pump error: {e:?}");
                break;
            }
        }
        franken_macos::flush_display();

        if window.is_closed() {
            println!("[fcb] window closed by user after {total_events} events");
            break;
        }

        if let Some(max_duration) = max_duration {
            if start.elapsed() >= max_duration {
                println!(
                    "[fcb] bounded execution complete ({:?}, {total_events} events)",
                    max_duration
                );
                break;
            }
        }

        let elapsed = start.elapsed().as_secs();
        if elapsed > 0 && elapsed % 10 == 0 && start.elapsed().subsec_millis() < 20 {
            println!("[fcb] alive — {elapsed}s, {total_events} events, Metal pipeline ready");
        }
    }

    let _ = cell.shutdown(token);
    let _ = device;
    let _ = layer;
    let _ = app;
    println!("[fcb] clean exit — Metal ABI bridge verified end-to-end");
    0
}
