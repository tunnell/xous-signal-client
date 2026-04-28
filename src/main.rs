#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

mod api;
use api::*;
use chat::{AuthorFlag, Chat, Event};
use enumset::EnumSet;
use gam::{MenuItem, MenuPayload};
use locales::t;
use num_traits::*;
use xous_signal_client::SigChat;
use xous_signal_client::manager::outgoing;
use xous_ipc::Buffer;

fn main() -> ! {
    let stack_size = 1024 * 1024;
    std::thread::Builder::new()
        .stack_size(stack_size)
        .spawn(wrapped_main)
        .unwrap()
        .join()
        .unwrap()
}

fn wrapped_main() -> ! {
    log_server::init_wait().unwrap();
    log::set_max_level(log::LevelFilter::Info);
    log::info!("my PID is {}", xous::process::id());

    const HEAP_LARGER_LIMIT: usize = 2048 * 1024;
    let new_limit = HEAP_LARGER_LIMIT;
    let result = xous::rsyscall(xous::SysCall::AdjustProcessLimit(
        xous::Limits::HeapMaximum as usize,
        0,
        new_limit,
    ));

    if let Ok(xous::Result::Scalar2(1, current_limit)) = result {
        xous::rsyscall(xous::SysCall::AdjustProcessLimit(
            xous::Limits::HeapMaximum as usize,
            current_limit,
            new_limit,
        ))
        .unwrap();
        log::info!("Heap limit increased to: {}", new_limit);
    } else {
        panic!("Unsupported syscall!");
    }

    let xns = xous_names::XousNames::new().unwrap();
    let sid = xns
        .register_name(SERVER_NAME_SIGCHAT, None)
        .expect("can't register server");
    log::trace!("registered with NS -- {:?}", sid);

    let chat = Chat::new(
        gam::APP_NAME_SIGCHAT,
        gam::APP_MENU_0_SIGCHAT,
        Some(xous::connect(sid).unwrap()),
        Some(SigchatOp::Post as usize),
        Some(SigchatOp::Event as usize),
        Some(SigchatOp::Rawkeys as usize),
    );

    let cid = xous::connect(sid).unwrap();
    chat.menu_add(MenuItem {
        name: t!("sigchat.menu.close", locales::LANG).to_string(),
        action_conn: Some(cid),
        action_opcode: SigchatOp::Menu as u32,
        action_payload: MenuPayload::Scalar([MenuOp::Noop as u32, 0, 0, 0]),
        close_on_select: true,
    })
    .expect("failed add menu");

    let mut sigchat = SigChat::new(&chat);
    let mut first_focus = true;
    let mut user_post: Option<String> = None;

    // Optional pre-seed of the V1 default outgoing recipient from the
    // environment. Lets a hosted-mode session send the first message
    // without first having received one. No-op if XSC_DEMO_PEER_UUID is
    // unset, invalid, or a recipient is already persisted.
    match outgoing::seed_demo_recipient_from_env() {
        Ok(true) => log::info!("seeded demo recipient from XSC_DEMO_PEER_UUID"),
        Ok(false) => {}
        Err(e) => log::warn!("seed_demo_recipient_from_env failed: {e}"),
    }

    // Auto-connect if the account is already registered (e.g. headless scan).
    // This fires the same logic as the first Event::Focus would have.
    if sigchat.is_ready() {
        first_focus = false;
        match sigchat.connect() {
            Ok(true) => {
                log::info!("connected to Signal Account");
                sigchat.dialogue_set(Some("default"));
                if let Err(e) = chat.set_author_flags("me", EnumSet::from(AuthorFlag::Right)) {
                    log::warn!("set_author_flags(\"me\") failed: {e:?}");
                }
                match sigchat.start_receive(chat.cid()) {
                    Ok(true) => log::info!("receive worker started"),
                    Ok(false) => log::warn!("start_receive returned false"),
                    Err(e) => log::warn!("start_receive failed: {e}"),
                }

            }
            Ok(false) => log::info!("not connected to Signal Account"),
            Err(e) => log::warn!("error while connecting to Signal Account: {e}"),
        }
    }

    loop {
        let msg = xous::receive_message(sid).unwrap();
        log::debug!("got message {:?}", msg);
        match FromPrimitive::from_usize(msg.body.id()) {
            Some(SigchatOp::Event) => {
                log::info!("got Chat UI Event");
                xous::msg_scalar_unpack!(msg, event_code, _, _, _, {
                    match FromPrimitive::from_usize(event_code) {
                        Some(Event::Focus) => {
                            if first_focus {
                                match sigchat.connect() {
                                    Ok(true) => {
                                        first_focus = false;
                                        log::info!("connected to Signal Account");
                                        sigchat.dialogue_set(Some("default"));
                                        if let Err(e) = chat.set_author_flags("me", EnumSet::from(AuthorFlag::Right)) {
                                            log::warn!("set_author_flags(\"me\") failed: {e:?}");
                                        }
                                        match sigchat.start_receive(chat.cid()) {
                                            Ok(true) => log::info!("receive worker started"),
                                            Ok(false) => log::warn!("start_receive returned false"),
                                            Err(e) => log::warn!("start_receive failed: {e}"),
                                        }
                                    }
                                    Ok(false) => log::info!("not connected to Signal Account"),
                                    Err(e) => {
                                        log::warn!("error while connecting to Signal Account: {e}")
                                    }
                                }
                            }
                            sigchat.redraw();
                        }
                        _ => (),
                    }
                });
            }
            Some(SigchatOp::Menu) => {
                log::info!("got Chat Menu Click");
                xous::msg_scalar_unpack!(msg, menu_code, _, _, _, {
                    match FromPrimitive::from_usize(menu_code) {
                        Some(MenuOp::Noop) => {}
                        _ => (),
                    }
                });
            }
            Some(SigchatOp::Post) => {
                let buffer =
                    unsafe { Buffer::from_memory_message(msg.body.memory_message().unwrap()) };
                let s = buffer.to_original::<String, _>().unwrap();
                log::info!("got SigchatOp::Post, s.len()={}", s.len());
                if s.len() > 0 {
                    // capture input instead of calling here, so message can drop and calling server is released
                    user_post = Some(s.to_string());
                }
            }
            Some(SigchatOp::Rawkeys) => log::info!("got sigchat rawkeys"),
            Some(SigchatOp::Quit) => {
                log::error!("got Quit");
                break;
            }
            _ => (),
        }
        if let Some(post) = user_post {
            sigchat.post(&post);
            user_post = None;
        }
    }
    // clean up our program
    log::error!("main loop exit, destroying servers");
    xns.unregister_server(sid).unwrap();
    xous::destroy_server(sid).unwrap();
    log::trace!("quitting");
    xous::terminate_process(0)
}
