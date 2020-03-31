use crate::{event::Event, transforms::Transform};
use futures01::{sync::mpsc::Receiver as FutureReceiver, Async, Stream as FutureStream};
use std::{
    sync::mpsc::{self, Receiver, Sender},
    thread::{self, JoinHandle},
    time::Duration,
};
use tokio01::timer::Interval;

#[derive(Clone, Copy)]
pub struct Timer {
    id: u32,
    interval_seconds: u64,
}

pub trait ScriptedRuntime {
    fn hook_init<F>(&mut self, _emit_fn: F) -> crate::Result<()>
    where
        F: FnMut(Event) -> (),
    {
        Ok(())
    }

    fn hook_process<F>(&mut self, _event: Event, _emit_fn: F) -> crate::Result<()>
    where
        F: FnMut(Event) -> ();

    fn hook_shutdown<F>(&mut self, _emit_fn: F) -> crate::Result<()>
    where
        F: FnMut(Event) -> (),
    {
        Ok(())
    }

    fn timer_handler<F>(&mut self, _timer: Timer, _emit_fn: F) -> crate::Result<()>
    where
        F: FnMut(Event) -> (),
    {
        Ok(())
    }

    fn timers(&self) -> Vec<Timer> {
        Vec::new()
    }
}

struct ScriptedTransform {
    input: Sender<Message>,
    output: Receiver<Option<Event>>,
    timers: Vec<Timer>,
}

enum Message {
    Init,
    Process(Event),
    Shutdown,
    Timer(Timer),
}

impl ScriptedTransform {
    fn new<F, T>(create_runtime: F) -> ScriptedTransform
    where
        F: FnOnce() -> T + Send + 'static,
        T: ScriptedRuntime,
    {
        let (input, runtime_input) = mpsc::channel();
        let (runtime_output, output) = mpsc::channel();

        // One-off channel to read statically defined list of timers from the runtime.
        let (timers_tx, timers_rx) = mpsc::sync_channel(0);

        thread::spawn(move || {
            let mut runtime = create_runtime();
            timers_tx.send(runtime.timers());

            for msg in runtime_input {
                match msg {
                    Message::Init => {
                        runtime.hook_init(|event| runtime_output.send(Some(event)).unwrap())
                    }
                    Message::Process(event) => runtime
                        .hook_process(event, |event| runtime_output.send(Some(event)).unwrap()),
                    Message::Shutdown => {
                        runtime.hook_shutdown(|event| runtime_output.send(Some(event)).unwrap())
                    }
                    Message::Timer(timer) => runtime
                        .timer_handler(timer, |event| runtime_output.send(Some(event)).unwrap()),
                };
                runtime_output.send(None).unwrap();
            }
        });

        ScriptedTransform {
            input,
            output,
            timers: timers_rx.recv().unwrap(),
        }
    }
}

impl Transform for ScriptedTransform {
    // used only in tests
    fn transform(&mut self, event: Event) -> Option<Event> {
        let mut out = Vec::new();
        self.transform_into(&mut out, event);
        assert!(out.len() <= 1);
        out.into_iter().next()
    }

    // used only in tests
    fn transform_into(&mut self, output: &mut Vec<Event>, event: Event) {
        self.input.send(Message::Process(event)).unwrap();
        while let Some(event) = self.output.recv().unwrap() {
            output.push(event);
        }
    }

    fn transform_stream(
        self: Box<Self>,
        input_rx: FutureReceiver<Event>,
    ) -> Box<dyn FutureStream<Item = Event, Error = ()> + Send>
    where
        Self: 'static,
    {
        Box::new(ScriptedStream::new(*self, input_rx))
    }
}

enum StreamState {
    Processing,
    Idle,
}

type MessageStream = Box<dyn FutureStream<Item = Message, Error = ()> + Send>;

struct ScriptedStream {
    transform: ScriptedTransform,
    input_rx: MessageStream,
    state: StreamState,
}

impl ScriptedStream {
    fn new(transform: ScriptedTransform, input_rx: FutureReceiver<Event>) -> ScriptedStream {
        let input_rx = input_rx.map(|event| Message::Process(event));
        let mut input_rx: MessageStream = Box::new(input_rx);
        for timer in transform.timers.iter() {
            input_rx = Box::new(input_rx.select(interval_from_timer(*timer)));
        }

        ScriptedStream {
            transform,
            input_rx,
            state: StreamState::Idle,
        }
    }
}

impl FutureStream for ScriptedStream {
    type Item = Event;
    type Error = ();

    fn poll(&mut self) -> Result<Async<Option<Self::Item>>, Self::Error> {
        match self.state {
            StreamState::Idle => match self.input_rx.poll() {
                Ok(Async::Ready(Some(msg))) => {
                    self.transform.input.send(msg).unwrap();
                    Ok(Async::Ready(None))
                }
                other => other.map(|_| Async::Ready(None)),
            },
            StreamState::Processing => match self.transform.output.try_recv() {
                Ok(Some(event)) => Ok(Async::Ready(Some(event))),
                Ok(None) => {
                    self.state = StreamState::Idle;
                    Ok(Async::Ready(None))
                }
                Err(_) => Ok(Async::Ready(None)),
            },
        }
    }
}

fn interval_from_timer(timer: Timer) -> impl FutureStream<Item = Message, Error = ()> + Send {
    Interval::new_interval(Duration::new(timer.interval_seconds, 0))
        .map(move |_| Message::Timer(timer))
        .map_err(|_| ())
}