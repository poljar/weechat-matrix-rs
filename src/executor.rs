use async_task::JoinHandle;
use pipe_channel::{channel, Receiver, Sender};
use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex, Weak};

use weechat::{FdHook, FdHookMode, Weechat};

static mut _EXECUTOR: Option<WeechatExecutor> = None;

type Job = async_task::Task<()>;
type FutureQueue = Arc<Mutex<VecDeque<Job>>>;
type FutureQueueHandle = Weak<Mutex<VecDeque<Job>>>;

pub struct WeechatExecutor {
    _hook: Option<FdHook<FutureQueueHandle, Receiver<()>>>,
    sender: Arc<Mutex<Sender<()>>>,
    futures: FutureQueue,
}

impl WeechatExecutor {
    fn new() -> Self {
        let weechat = unsafe { Weechat::weechat() };

        let (sender, receiver) = channel();
        let sender = Arc::new(Mutex::new(sender));
        let queue = Arc::new(Mutex::new(VecDeque::new()));

        let hook = weechat
            .hook_fd(
                receiver,
                FdHookMode::Read,
                WeechatExecutor::executor_cb,
                Some(Arc::downgrade(&queue)),
            )
            .expect("Can't create executor FD hook");

        WeechatExecutor {
            _hook: Some(hook),
            sender,
            futures: queue,
        }
    }

    fn executor_cb(future_queue: &FutureQueueHandle, receiver: &mut Receiver<()>) {
        if receiver.recv().is_err() {
            return;
        }

        let futures = future_queue.upgrade();

        if let Some(q) = futures {
            let mut futures = q.lock().unwrap();
            let task = futures.pop_front();

            // Drop the lock here so we can spawn new futures from the currently
            // running one.
            drop(futures);

            if let Some(task) = task {
                task.run();
            }
        }
    }

    pub fn free() {
        unsafe {
            _EXECUTOR.take();
        }
    }
}

pub fn spawn_weechat<F, R>(future: F) -> JoinHandle<R, ()>
where
    F: Future<Output = R> + 'static,
    R: 'static,
{
    let executor = unsafe {
        if _EXECUTOR.is_none() {
            let executor = WeechatExecutor::new();
            _EXECUTOR = Some(executor);
        }

        _EXECUTOR.as_ref().expect("Executor wasn't started")
    };

    let sender = Arc::downgrade(&executor.sender);
    let queue = Arc::downgrade(&executor.futures);

    let schedule = move |task| {
        let sender = sender.upgrade();
        let queue = queue.upgrade();

        if let Some(q) = queue {
            let sender = sender
                .expect("Futures queue exists but the channel got dropped");
            let mut weechat_notify = sender
                .lock()
                .expect("Weechat notification sender lock is poisoned");

            let mut queue = q.lock().expect(
                "Lock of the future queue of the Weechat executor is poisoned",
            );

            queue.push_back(task);
            weechat_notify
                .send(())
                .expect("Can't notify Weechat to run a future");
        }
    };

    let (task, handle) = async_task::spawn_local(future, schedule, ());

    task.schedule();

    handle
}
