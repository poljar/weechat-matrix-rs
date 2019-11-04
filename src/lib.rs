use std::time::Duration;
use std::sync::{Arc, Mutex};

use pipe_channel::{channel, Sender, Receiver};
use tokio::runtime::Runtime;

use async_task;
use async_std;
use async_std::sync::channel as async_channel;
use async_std::sync::Sender as AsyncSender;
use std::future::Future;
use std::collections::VecDeque;

use weechat::{weechat_plugin, ArgsWeechat, Weechat, WeechatPlugin, WeechatResult,
              FdHookMode, FdHook};

use matrix_nio::{
    self,
    events::{
        collections::all::RoomEvent,
        room::message::{MessageEvent, MessageEventContent, TextMessageEventContent},
    },
    AsyncClient,
    SyncSettings,
    AsyncClientConfig,
};

static mut _WEECHAT: Option<Weechat> = None;

struct SamplePlugin {
    weechat: Weechat,
    tokio: Option<Runtime>,
}

fn spawn_cb(future_queue: &FutureQueue, receiver: &mut Receiver<()>) {
    receiver.recv().unwrap();

    let mut queue = future_queue.lock().unwrap();
    let task = queue.pop_front();

    if let Some(task) = task {
        task.run();
    }
}


type Job = async_task::Task<()>;

static mut _FUTURE_HOOK: Option<FdHook<FutureQueue, Receiver<()>>> = None;
static mut _SENDER: Option<Arc<Mutex<Sender<()>>>> = None;
static mut _FUTURE_QUEUE: Option<FutureQueue> = None;

type FutureQueue = Arc<Mutex<VecDeque<Job>>>;

fn spawn_weechat<F, T>(future: F)
where
    F: Future<Output = T> + 'static,
    T: 'static
{
    let weechat = get_weechat();

    unsafe {
        if _FUTURE_HOOK.is_none() {
            let (sender, receiver) = channel();
            let sender = Arc::new(Mutex::new(sender));
            _SENDER = Some(sender);
            let queue = Arc::new(Mutex::new(VecDeque::new()));
            _FUTURE_QUEUE = Some(queue.clone());

            let fd_hook = weechat.hook_fd(
                receiver,
                FdHookMode::Read,
                spawn_cb,
                Some(queue)
            );
            _FUTURE_HOOK = Some(fd_hook);
        }
    }

    let weechat_notify = unsafe {
        if let Some(s) = &_SENDER {
            s.clone()
        } else {
            panic!("Future queue wasn't initialized")
        }
    };

    let queue: FutureQueue = unsafe {
        if let Some(q) = &_FUTURE_QUEUE {
            q.clone()
        } else {
            panic!("Future queue wasn't initialized")
        }
    };

    let schedule = move |task| {
         let mut weechat_notify = weechat_notify.lock().unwrap();
         let mut queue = queue.lock().unwrap();

         queue.push_back(task);
         weechat_notify.send(()).unwrap();
    };

    let (task, _handle) = async_task::spawn(future, schedule, ());
    task.schedule();
}


fn get_weechat() -> &'static mut Weechat {
    unsafe {
        match &mut _WEECHAT {
            Some(x) => x,
            None => panic!(),
        }
    }
}

fn get_plugin() -> &'static mut SamplePlugin {
    unsafe {
        match &mut __PLUGIN {
            Some(x) => x,
            None => panic!(),
        }
    }
}

async fn sync_loop(channel: AsyncSender<Result<(String, String), String>>) {
    let server = "http://localhost:8008";
    let config = AsyncClientConfig::new().proxy("http://localhost:8080").unwrap().disable_ssl_verification();
    let mut client = AsyncClient::new_with_config(server, None, config).unwrap();

    channel.send(Err("HELLO WORLD".to_string())).await;
    channel.send(Err("HELLO WORLD".to_string())).await;

    let ret = client.login("example", "wordpass", None).await;

    match ret {
        Ok(_) => (),
        Err(e) => {
            channel.send(Err("No logging in".to_string())).await;
            return;
        },
    }

    channel.send(Err("Syncing now".to_string())).await;

    let sync_settings = SyncSettings::new().timeout(30000).unwrap();
    let response = client.sync(sync_settings).await;

    channel.send(Err("Synced now".to_string())).await;

    match response {
        Ok(r) => {
            for (room_id, room) in r.rooms.join {
                for event in room.timeline.events {
                    let event = match event.into_result() {
                        Ok(e) => e,
                        Err(e) => continue,
                    };
                    if let RoomEvent::RoomMessage(MessageEvent {
                    content:
                        MessageEventContent::Text(TextMessageEventContent { body: msg_body, .. }),
                    sender,
                    ..
                    }) = event
                    {
                        channel.send(Ok((sender.to_string(), msg_body.to_string()))).await;
                    }
                }
            }
        },
        Err(e) => {
            let err = format!("{:?}", e.to_string());
            channel.send(Err(err)).await;
            ()
        }
    }

    loop {
        channel.send(Ok(("Hello".to_string(), "world".to_string()))).await;
        async_std::task::sleep(Duration::from_secs(3)).await;
    }

}

impl WeechatPlugin for SamplePlugin {
    fn init(weechat: Weechat, _args: ArgsWeechat) -> WeechatResult<Self> {
        unsafe {
            _WEECHAT = Some(weechat.clone());
        }

        let runtime = Runtime::new().unwrap();
        let (tx, rx) = async_channel(100);

        let weechat_task = async move {
            let weechat = get_weechat();
            weechat.print("Hello async/await");
            let plugin = get_plugin();

            loop {
                let ret = match rx.recv().await {
                    Some(m) => m,
                    None => {
                        weechat.print("Error receiving message");
                        return;
                    }
                };

                match ret {
                    Ok((sender, msg)) => {
                        weechat.print(&format!("Got message from {}: {}", sender, msg));
                    },
                    Err(e) => weechat.print(&format!("Ruma error {}", e)),
                };
            }
        };

        runtime.spawn(async move {
            sync_loop(tx).await;
        });

        spawn_weechat(weechat_task);

        Ok(SamplePlugin { weechat, tokio: Some(runtime) })
    }
}

impl Drop for SamplePlugin {
    fn drop(&mut self) {
        let runtime = self.tokio.take();

        if let Some(r) = runtime {
            r.shutdown_now();
        }

        self.weechat.print("Bye rust!");
        unsafe {
            _FUTURE_HOOK.take();
            _SENDER.take();
            _FUTURE_QUEUE.take();
        }
    }
}

weechat_plugin!(
    SamplePlugin,
    name: "weechat-matrix",
    author: "poljar",
    description: "",
    version: "0.1.0",
    license: "MIT"
);
