use async_task;
use pipe_channel::{channel, Receiver, Sender};
use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};

use weechat::{FdHook, FdHookMode, Weechat};

fn spawn_cb(future_queue: &FutureQueue, receiver: &mut Receiver<()>) {
    receiver
        .recv()
        .expect("Executor channel has been dropped before unhooking");
    let mut queue = future_queue
        .lock()
        .expect("Future queue has been dropped before unhooking");

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

pub fn spawn_weechat<F, T>(future: F)
where
    F: Future<Output = T> + 'static,
    T: 'static,
{
    let weechat = unsafe { Weechat::weechat() };

    unsafe {
        if _FUTURE_HOOK.is_none() {
            let (sender, receiver) = channel();
            let sender = Arc::new(Mutex::new(sender));
            let queue = Arc::new(Mutex::new(VecDeque::new()));

            _SENDER = Some(sender);
            _FUTURE_QUEUE = Some(queue.clone());

            let fd_hook = weechat.hook_fd(
                receiver,
                FdHookMode::Read,
                spawn_cb,
                Some(queue),
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

pub fn cleanup_executor() {
    unsafe {
        let hook = _FUTURE_HOOK.take();
        // Drop our fd hook so it doesn't get called because we dropped the
        // sender.
        drop(hook);

        _SENDER.take();
        _FUTURE_QUEUE.take();
    }
}
