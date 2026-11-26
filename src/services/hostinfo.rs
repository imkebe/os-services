//! The host info service. The KVM shows what the target computer is and
//! what it is doing: an inventory, live metrics while somebody watches, and
//! the process, top and USB lists on request. The documents come from the
//! `ipkvm-hostinfo` crate, which reads them with the user's own rights.
//!
//! The processes are read only to answer a query for the process list or
//! the top list, once for each such query. The metrics of a subscription
//! never read them: that was the costly part of a sample.
//!
//! Collection has its own thread: a sample takes milliseconds to a second,
//! and the threads that read and write the USB link must not wait for it.
//! The service runs only while the KVM's session.config turns it on; the
//! link hands it the KVM's requests, and it sends through `sink`, on
//! whatever thread owns the wire.

use crate::protocol::{
    HostInfoReply, HOSTINFO_INVENTORY_SECTION, HOSTINFO_MAX_CHUNK, HOSTINFO_MAX_DOCUMENT,
    HOSTINFO_MAX_RESULT, HOSTINFO_METRICS_SECTION, HOSTINFO_PROCESSES_SECTION,
    HOSTINFO_QUERY_INVENTORY, HOSTINFO_QUERY_PROCESSES, HOSTINFO_QUERY_TOP, HOSTINFO_QUERY_USB,
    HOSTINFO_USB_SECTION,
};
use ipkvm_hostinfo::{to_json, Collector};
use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

/// Every section: this sender offers them all on every OS, and leaves empty
/// what an OS does not tell.
const SECTIONS: u32 = HOSTINFO_INVENTORY_SECTION
    | HOSTINFO_METRICS_SECTION
    | HOSTINFO_PROCESSES_SECTION
    | HOSTINFO_USB_SECTION;
/// The shortest time between two hostinfo.metrics.
const MIN_INTERVAL: Duration = Duration::from_millis(500);
/// How long a subscription lasts after the KVM's last hostinfo.subscribe.
/// The KVM renews every 5 seconds while somebody watches.
const LEASE: Duration = Duration::from_secs(15);
/// How often the inventory is read again; it is sent only when it changed.
const INVENTORY_INTERVAL: Duration = Duration::from_secs(30);
/// How long the thread sleeps while it has nothing due.
const IDLE: Duration = Duration::from_secs(3600);

/// What the service puts on the wire.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Output {
    /// hostinfo.capabilities with these sections; zero withdraws.
    Register(u32),
    Reply(HostInfoReply),
}

enum Command {
    Enable(bool),
    Subscribe { interval_ms: u32 },
    Query { request_id: u32, section: u8 },
    Stop,
}

/// The session's handle on the collection thread.
pub struct HostInfo {
    commands: mpsc::Sender<Command>,
    thread: Option<thread::JoinHandle<()>>,
}

impl HostInfo {
    /// Starts the thread. With `enabled`, the policy of the session.config
    /// the session opened with, it registers the service at once.
    pub fn spawn(enabled: bool, sink: impl Fn(Output) + Send + 'static) -> Self {
        let (commands, received) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("hostinfo".into())
            .spawn(move || Service::new(sink).run(&received, enabled))
            .expect("spawn the host info thread");
        Self {
            commands,
            thread: Some(thread),
        }
    }

    /// The policy of a session.config. Turning the service on registers it
    /// and sends the inventory; turning it off withdraws it.
    pub fn set_enabled(&self, enabled: bool) {
        self.send(Command::Enable(enabled));
    }

    /// hostinfo.subscribe.
    pub fn subscribe(&self, interval_ms: u32) {
        self.send(Command::Subscribe { interval_ms });
    }

    /// hostinfo.query.
    pub fn query(&self, request_id: u32, section: u8) {
        self.send(Command::Query {
            request_id,
            section,
        });
    }

    /// Ends the service without a word to the KVM: nothing more is sent.
    pub fn stop(&self) {
        self.send(Command::Stop);
    }

    fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

impl Drop for HostInfo {
    /// Waits for the thread, which finishes the sample it is taking first.
    fn drop(&mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A live metrics subscription.
struct Subscription {
    interval: Duration,
    expires: Instant,
    /// When the next hostinfo.metrics goes out.
    due: Instant,
}

/// The service as its thread runs it. It is on while it has a collector.
struct Service<S> {
    sink: S,
    collector: Option<Collector>,
    subscription: Option<Subscription>,
    /// The inventory the KVM has.
    inventory: Option<String>,
    inventory_due: Instant,
}

impl<S: Fn(Output)> Service<S> {
    fn new(sink: S) -> Self {
        Self {
            sink,
            collector: None,
            subscription: None,
            inventory: None,
            inventory_due: Instant::now(),
        }
    }

    fn run(mut self, commands: &mpsc::Receiver<Command>, enabled: bool) {
        self.set_enabled(enabled);
        loop {
            self.tick(Instant::now());
            let wait = self.next_wake().saturating_duration_since(Instant::now());
            match commands.recv_timeout(wait) {
                Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Ok(command) => self.handle(command),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Enable(enabled) => self.set_enabled(enabled),
            Command::Subscribe { interval_ms } => self.subscribe(interval_ms, Instant::now()),
            Command::Query {
                request_id,
                section,
            } => self.query(request_id, section),
            Command::Stop => {}
        }
    }

    fn reply(&self, reply: HostInfoReply) {
        (self.sink)(Output::Reply(reply));
    }

    /// A session.config that changes nothing sends nothing.
    fn set_enabled(&mut self, enabled: bool) {
        if enabled == self.collector.is_some() {
            return;
        }
        self.subscription = None;
        self.inventory = None;
        if enabled {
            self.collector = Some(Collector::new());
            self.inventory_due = Instant::now();
            (self.sink)(Output::Register(SECTIONS));
        } else {
            self.collector = None;
            (self.sink)(Output::Register(0));
        }
    }

    /// Starts, renews or ends the subscription. A renewal keeps the pace:
    /// the next metrics go out when they were due anyway. A new
    /// subscription starts its first interval now.
    fn subscribe(&mut self, interval_ms: u32, now: Instant) {
        let Some(collector) = &mut self.collector else {
            return;
        };
        if interval_ms == 0 {
            self.subscription = None;
            return;
        }
        let interval = Duration::from_millis(interval_ms.into()).max(MIN_INTERVAL);
        let due = match &self.subscription {
            Some(current) if current.interval == interval => current.due,
            Some(current) => current.due.min(now + interval),
            None => {
                collector.prime();
                now + interval
            }
        };
        self.subscription = Some(Subscription {
            interval,
            expires: now + LEASE,
            due,
        });
    }

    /// Answers a query in chunks. A service that is off, a section this
    /// version does not know and an answer over the limit get an error.
    /// A query for the process list or the top list refreshes the
    /// processes once; nothing else in the service does.
    fn query(&mut self, request_id: u32, section: u8) {
        let answer = match (&mut self.collector, section) {
            (None, _) => Err("host info is turned off".to_owned()),
            (Some(collector), HOSTINFO_QUERY_INVENTORY) => Ok(to_json(&collector.inventory())),
            (Some(collector), HOSTINFO_QUERY_PROCESSES) => Ok(to_json(&collector.processes())),
            (Some(collector), HOSTINFO_QUERY_USB) => Ok(to_json(&collector.usb_devices())),
            (Some(collector), HOSTINFO_QUERY_TOP) => Ok(to_json(&collector.top())),
            (Some(_), _) => Err(format!("unknown host info section {section}")),
        };
        let answer = answer.and_then(|json| match json.len() {
            length if length > HOSTINFO_MAX_RESULT => {
                Err(format!("the answer is too large: {length} bytes"))
            }
            _ => Ok(json),
        });
        let (body, error) = match &answer {
            Ok(json) => (json.as_bytes(), false),
            Err(message) => (message.as_bytes(), true),
        };
        for chunk in result_chunks(request_id, section, body, error) {
            self.reply(chunk);
        }
    }

    /// Sends what is due: the inventory when it changed, and the metrics of
    /// a subscription that has not expired.
    fn tick(&mut self, now: Instant) {
        let Some(collector) = &mut self.collector else {
            return;
        };
        if now >= self.inventory_due {
            let inventory = collector.inventory().to_json_within(HOSTINFO_MAX_DOCUMENT);
            self.inventory_due = Instant::now() + INVENTORY_INTERVAL;
            if self.inventory.as_ref() != Some(&inventory) {
                self.inventory = Some(inventory.clone());
                (self.sink)(Output::Reply(HostInfoReply::Inventory(inventory)));
            }
        }
        if self
            .subscription
            .as_ref()
            .is_some_and(|sub| now >= sub.expires)
        {
            self.subscription = None;
        }
        let Some(subscription) = &mut self.subscription else {
            return;
        };
        if now < subscription.due {
            return;
        }
        let metrics = collector.metrics();
        // A sample that took longer than the interval is not made up for.
        subscription.due = (subscription.due + subscription.interval).max(Instant::now());
        let metrics = metrics.to_json_within(HOSTINFO_MAX_DOCUMENT);
        (self.sink)(Output::Reply(HostInfoReply::Metrics(metrics)));
    }

    /// When the thread next has something to do without a command.
    fn next_wake(&self) -> Instant {
        if self.collector.is_none() {
            return Instant::now() + IDLE;
        }
        match &self.subscription {
            Some(subscription) => self
                .inventory_due
                .min(subscription.due)
                .min(subscription.expires),
            None => self.inventory_due,
        }
    }
}

/// The hostinfo.result messages that carry `body`: chunks of at most
/// 60 KiB, the last one flagged. An empty body is one empty last chunk.
fn result_chunks(request_id: u32, section: u8, body: &[u8], error: bool) -> Vec<HostInfoReply> {
    let mut chunks: Vec<&[u8]> = body.chunks(HOSTINFO_MAX_CHUNK).collect();
    if chunks.is_empty() {
        chunks.push(&[]);
    }
    let last = chunks.len() - 1;
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| HostInfoReply::Result {
            request_id,
            section,
            last: index == last,
            error,
            body: chunk.to_vec(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const PATIENCE: Duration = Duration::from_secs(20);

    fn service(enabled: bool) -> (HostInfo, mpsc::Receiver<Output>) {
        let (sent, outputs) = mpsc::channel();
        let service = HostInfo::spawn(enabled, move |output| {
            let _ = sent.send(output);
        });
        (service, outputs)
    }

    fn next(outputs: &mpsc::Receiver<Output>) -> Output {
        outputs.recv_timeout(PATIENCE).expect("an output")
    }

    fn json(text: &str) -> Value {
        serde_json::from_str(text).expect("JSON")
    }

    /// The whole answer to a query: its body and its error flag.
    fn answer(outputs: &mpsc::Receiver<Output>, id: u32, asked: u8) -> (Vec<u8>, bool) {
        let mut whole = Vec::new();
        loop {
            match next(outputs) {
                Output::Reply(HostInfoReply::Result {
                    request_id,
                    section,
                    last,
                    error,
                    body,
                }) => {
                    assert_eq!((request_id, section), (id, asked));
                    assert!(body.len() <= HOSTINFO_MAX_CHUNK);
                    whole.extend_from_slice(&body);
                    if last {
                        return (whole, error);
                    }
                }
                // Metrics and a changed inventory may come in between.
                Output::Reply(_) => {}
                other => panic!("not a result: {other:?}"),
            }
        }
    }

    #[test]
    fn results_are_cut_into_chunks_and_the_last_is_flagged() {
        let body = vec![b'x'; HOSTINFO_MAX_CHUNK * 2 + 5];
        let chunks = result_chunks(7, 2, &body, false);
        let sizes: Vec<(usize, bool)> = chunks
            .iter()
            .map(|chunk| match chunk {
                HostInfoReply::Result { body, last, .. } => (body.len(), *last),
                other => panic!("not a result: {other:?}"),
            })
            .collect();
        assert_eq!(
            sizes,
            [
                (HOSTINFO_MAX_CHUNK, false),
                (HOSTINFO_MAX_CHUNK, false),
                (5, true)
            ]
        );
        let exact = result_chunks(7, 2, &body[..HOSTINFO_MAX_CHUNK], false);
        assert_eq!(exact.len(), 1);
        let empty = HostInfoReply::Result {
            request_id: 7,
            section: 3,
            last: true,
            error: true,
            body: Vec::new(),
        };
        assert_eq!(result_chunks(7, 3, &[], true), [empty]);
    }

    #[test]
    fn registers_and_sends_the_inventory_once_when_turned_on() {
        let (service, outputs) = service(true);
        assert_eq!(next(&outputs), Output::Register(15));
        let Output::Reply(HostInfoReply::Inventory(inventory)) = next(&outputs) else {
            panic!("the inventory follows the registration");
        };
        assert!(inventory.len() <= HOSTINFO_MAX_DOCUMENT);
        assert_eq!(json(&inventory)["schema"], 1);
        // The same policy again changes nothing, and no metrics flow
        // without a subscription.
        service.set_enabled(true);
        assert!(outputs.recv_timeout(Duration::from_millis(700)).is_err());
        service.set_enabled(false);
        assert_eq!(next(&outputs), Output::Register(0));
    }

    #[test]
    fn a_service_that_is_off_says_nothing_and_refuses_queries() {
        let (service, outputs) = service(false);
        service.subscribe(500);
        assert!(outputs.recv_timeout(Duration::from_millis(700)).is_err());
        service.query(4, HOSTINFO_QUERY_USB);
        let (message, error) = answer(&outputs, 4, HOSTINFO_QUERY_USB);
        assert!(error);
        assert_eq!(message, b"host info is turned off");
    }

    #[test]
    fn answers_queries_for_each_section() {
        let (service, outputs) = service(true);
        assert_eq!(next(&outputs), Output::Register(15));
        for (id, section, key) in [
            (1, HOSTINFO_QUERY_INVENTORY, "os"),
            (2, HOSTINFO_QUERY_PROCESSES, "processes"),
            (3, HOSTINFO_QUERY_USB, "devices"),
            (4, HOSTINFO_QUERY_TOP, "top"),
        ] {
            service.query(id, section);
            let (body, error) = answer(&outputs, id, section);
            assert!(!error, "section {section}");
            let document = json(std::str::from_utf8(&body).unwrap());
            assert_eq!(document["schema"], 1);
            assert!(document.get(key).is_some(), "section {section}");
        }
        service.query(9, 42);
        let (message, error) = answer(&outputs, 9, 42);
        assert!(error);
        assert_eq!(message, b"unknown host info section 42");
    }

    #[test]
    fn metrics_flow_while_subscribed_at_no_less_than_the_shortest_interval() {
        let (service, outputs) = service(true);
        assert_eq!(next(&outputs), Output::Register(15));
        assert!(matches!(
            next(&outputs),
            Output::Reply(HostInfoReply::Inventory(_))
        ));
        let subscribed = Instant::now();
        service.subscribe(1);
        let mut arrivals = Vec::new();
        while arrivals.len() < 3 {
            if let Output::Reply(HostInfoReply::Metrics(metrics)) = next(&outputs) {
                let metrics = json(&metrics);
                assert_eq!(metrics["schema"], 1);
                // The metrics read no process: no top list, and no count
                // before a query asked for the processes.
                assert!(metrics.get("top").is_none());
                assert_eq!(metrics["process_count"], 0);
                arrivals.push(subscribed.elapsed());
            }
        }
        assert!(arrivals[0] >= MIN_INTERVAL, "{arrivals:?}");
        assert!(arrivals[2] >= MIN_INTERVAL * 3, "{arrivals:?}");
        // Metrics go on after a renewal; a zero interval ends the flow.
        service.subscribe(1);
        assert!(matches!(
            next(&outputs),
            Output::Reply(HostInfoReply::Metrics(_))
        ));
        service.subscribe(0);
        thread::sleep(MIN_INTERVAL);
        while outputs.try_recv().is_ok() {}
        assert!(outputs.recv_timeout(MIN_INTERVAL * 3).is_err());
    }

    /// The processes are read by the queries for them and by nothing else:
    /// the metrics carry the count of the last such query, and each query
    /// measures its loads from the query before it.
    #[test]
    fn only_process_queries_read_the_processes() {
        let (sent, outputs) = mpsc::channel();
        let mut service = Service::new(move |output| {
            let _ = sent.send(output);
        });
        service.set_enabled(true);
        let document = |outputs: &mpsc::Receiver<Output>, id: u32, section: u8| {
            let (body, error) = answer(outputs, id, section);
            assert!(!error);
            json(std::str::from_utf8(&body).unwrap())
        };
        let metrics = |service: &mut Service<_>| {
            service.subscribe(500, Instant::now());
            let due = service.subscription.as_ref().unwrap().due;
            service.tick(due);
            loop {
                if let Output::Reply(HostInfoReply::Metrics(metrics)) = next(&outputs) {
                    return json(&metrics);
                }
            }
        };
        assert_eq!(metrics(&mut service)["process_count"], 0);
        assert_eq!(metrics(&mut service)["process_count"], 0);

        service.query(1, HOSTINFO_QUERY_TOP);
        let top = document(&outputs, 1, HOSTINFO_QUERY_TOP);
        assert_eq!(top["window_s"], 0.0, "the first refresh has no window");
        let count = top["process_count"].as_u64().unwrap();
        assert!(count > 0);
        assert!(top["top"].as_array().unwrap().len() <= 10);
        assert_eq!(metrics(&mut service)["process_count"], count);

        thread::sleep(Duration::from_millis(300));
        // Metrics in between do not shorten the window of the next query.
        metrics(&mut service);
        service.query(2, HOSTINFO_QUERY_PROCESSES);
        let list = document(&outputs, 2, HOSTINFO_QUERY_PROCESSES);
        let window = list["window_s"].as_f64().unwrap();
        assert!((0.3..10.0).contains(&window), "{window}");
        let listed = list["processes"].as_array().unwrap();
        assert!(listed[0].get("cpu_time_ms").is_some());
        assert_eq!(metrics(&mut service)["process_count"], listed.len() as u64);
    }

    #[test]
    fn a_subscription_expires_without_renewal() {
        let (sent, outputs) = mpsc::channel();
        let mut service = Service::new(move |output| {
            let _ = sent.send(output);
        });
        service.set_enabled(true);
        let start = Instant::now();
        service.subscribe(1000, start);
        service.inventory_due = start + IDLE;
        assert_eq!(service.next_wake(), start + Duration::from_secs(1));
        // A renewal extends the lease and keeps the pace.
        service.subscribe(1000, start + Duration::from_millis(400));
        let subscription = service.subscription.as_ref().unwrap();
        assert_eq!(subscription.due, start + Duration::from_secs(1));
        assert_eq!(
            subscription.expires,
            start + Duration::from_millis(400) + LEASE
        );
        service.tick(start + LEASE + Duration::from_millis(399));
        assert!(service.subscription.is_some());
        while outputs.try_recv().is_ok() {}
        service.tick(start + LEASE + Duration::from_millis(400));
        assert!(service.subscription.is_none());
        assert!(
            outputs.try_recv().is_err(),
            "an expired lease sends nothing"
        );
        assert_eq!(service.next_wake(), start + IDLE);
    }
}
