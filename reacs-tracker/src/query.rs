use crate::graph::{OUTSIDE, instance_line, sender_name, short_name};
use crate::source::Source;
use reacs::tracking::{ReceivedCount, TrackedInstance, TrackedSignalFrame, TrackingSave};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

const DEFAULT_LIMIT: usize = 200;
const MAX_IDS_SHOWN: usize = 8;

pub const QUERY_USAGE: &str = "reacs-tracker report <file>
reacs-tracker query <file> [--tick N | A..B] [--signal NAME] [--system NAME] [--entity 50v1 | 50]
                           [--id N] [--kind sent|read|cancelled|modified] [--unread] [--limit N, 0 for all]";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Sent,
    Read,
    Cancelled,
    Modified,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Sent => "sent",
            Kind::Read => "read",
            Kind::Cancelled => "cancelled",
            Kind::Modified => "modified",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [Kind::Sent, Kind::Read, Kind::Cancelled, Kind::Modified]
            .into_iter()
            .find(|kind| kind.name() == text)
    }
}

pub struct Event<'a> {
    tick: u64,
    kind: Kind,
    signal: u128,
    system: u128,
    sender: u128,
    sent_tick: Option<u64>,
    windowed: bool,
    count: u64,
    id: Option<u64>,
    instances: Vec<&'a TrackedInstance>,
    payload: Option<&'a str>,
}

fn id_list(instances: &[&TrackedInstance]) -> String {
    let mut text = instances
        .iter()
        .take(MAX_IDS_SHOWN)
        .map(|instance| format!("#{}", instance.id))
        .collect::<Vec<_>>()
        .join(",");
    if instances.len() > MAX_IDS_SHOWN {
        text.push_str(&format!(",+{}", instances.len() - MAX_IDS_SHOWN));
    }
    text
}

impl Event<'_> {
    fn origin(&self, save: &TrackingSave) -> String {
        let sender = sender_name(save, self.sender);
        match self.sent_tick {
            Some(tick) => format!("from={sender}@{tick}"),
            None => format!("from={sender}"),
        }
    }

    fn detail(&self, save: &TrackingSave) -> String {
        match self.kind {
            Kind::Sent => self
                .instances
                .first()
                .map_or_else(String::new, |x| instance_line(x, self.tick)),
            Kind::Read => {
                let origin = self.origin(save);
                let of = match self.instances.len() as u64 {
                    0 => String::new(),
                    listed if listed == self.count => String::new(),
                    listed => format!(" of={listed}"),
                };
                let ids = id_list(&self.instances);
                let window = if self.windowed { " window" } else { "" };
                format!("{ids} count={}{of} {origin}{window}", self.count)
                    .trim_start()
                    .to_string()
            }
            Kind::Cancelled => format!("#{} {}", self.id.unwrap_or(0), self.origin(save)),
            Kind::Modified => format!(
                "#{} now={} {}",
                self.id.unwrap_or(0),
                self.payload.unwrap_or("?"),
                self.origin(save)
            ),
        }
    }

    pub fn line(&self, save: &TrackingSave) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}",
            self.tick,
            self.kind.name(),
            short_name(save.signal_name(self.signal)),
            sender_name(save, self.system),
            self.detail(save)
        )
    }

    fn mentions_id(&self, id: u64) -> bool {
        self.id == Some(id) || self.instances.iter().any(|x| x.id == id)
    }

    fn mentions_entity(&self, entity: &EntityFilter) -> bool {
        self.instances
            .iter()
            .flat_map(|x| &x.entities)
            .any(|bits| entity.matches(*bits))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntityFilter {
    Index(u32),
    Exact(u64),
}

impl EntityFilter {
    fn parse(text: &str) -> Option<Self> {
        match text.split_once('v') {
            Some((index, generation)) => {
                let index: u32 = index.parse().ok()?;
                let generation: u32 = generation.parse().ok()?;
                Some(EntityFilter::Exact(
                    (u64::from(generation) << 32) | u64::from(index),
                ))
            }
            None => text.parse().ok().map(EntityFilter::Index),
        }
    }

    fn matches(&self, bits: u64) -> bool {
        match *self {
            EntityFilter::Index(index) => bits as u32 == index,
            EntityFilter::Exact(exact) => bits == exact,
        }
    }
}

fn parse_ticks(text: &str) -> Option<Range<u64>> {
    match text.split_once("..") {
        Some((start, end)) => {
            let start = if start.is_empty() {
                0
            } else {
                start.parse().ok()?
            };
            let end = if end.is_empty() {
                u64::MAX
            } else {
                end.parse().ok()?
            };
            Some(start..end)
        }
        None => {
            let tick: u64 = text.parse().ok()?;
            Some(tick..tick.saturating_add(1))
        }
    }
}

#[derive(Default)]
pub struct QueryArgs {
    ticks: Option<Range<u64>>,
    signal: Option<String>,
    system: Option<String>,
    entity: Option<EntityFilter>,
    id: Option<u64>,
    kind: Option<Kind>,
    unread: bool,
    limit: Option<usize>,
}

impl QueryArgs {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut query = Self::default();
        let mut args = args.iter();
        while let Some(flag) = args.next() {
            if flag == "--unread" {
                query.unread = true;
                continue;
            }
            let value = args
                .next()
                .filter(|_| flag.starts_with("--"))
                .ok_or_else(|| format!("`{flag}` needs a value\n{QUERY_USAGE}"))?;
            let invalid = || format!("bad value for `{flag}`: `{value}`\n{QUERY_USAGE}");
            match flag.as_str() {
                "--tick" => query.ticks = Some(parse_ticks(value).ok_or_else(invalid)?),
                "--signal" => query.signal = Some(value.clone()),
                "--system" => query.system = Some(value.clone()),
                "--entity" => query.entity = Some(EntityFilter::parse(value).ok_or_else(invalid)?),
                "--id" => query.id = Some(value.parse().map_err(|_| invalid())?),
                "--kind" => query.kind = Some(Kind::parse(value).ok_or_else(invalid)?),
                "--limit" => query.limit = Some(value.parse().map_err(|_| invalid())?),
                unknown => return Err(format!("unknown option `{unknown}`\n{QUERY_USAGE}")),
            }
        }
        Ok(query)
    }
}

fn resolve_names(
    names: Vec<(u128, String)>,
    needle: &str,
    what: &str,
) -> Result<BTreeSet<u128>, String> {
    let exact: BTreeSet<u128> = names
        .iter()
        .filter(|(_, name)| name == needle || short_name(name) == needle)
        .map(|(id, _)| *id)
        .collect();
    if !exact.is_empty() {
        return Ok(exact);
    }
    let partial: BTreeSet<u128> = names
        .iter()
        .filter(|(_, name)| name.contains(needle))
        .map(|(id, _)| *id)
        .collect();
    if !partial.is_empty() {
        return Ok(partial);
    }
    let known = names
        .iter()
        .map(|(_, name)| short_name(name))
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!("no {what} matches `{needle}`, known: {known}"))
}

fn signal_names(save: &TrackingSave) -> Vec<(u128, String)> {
    save.signal_ids()
        .map(|id| (id, save.signal_name(id).to_string()))
        .collect()
}

fn system_names(save: &TrackingSave) -> Vec<(u128, String)> {
    save.system_order()
        .iter()
        .copied()
        .chain([OUTSIDE])
        .map(|id| (id, save.system_name(id).to_string()))
        .collect()
}

struct Filter {
    kind: Option<Kind>,
    signals: Option<BTreeSet<u128>>,
    systems: Option<BTreeSet<u128>>,
    entity: Option<EntityFilter>,
    id: Option<u64>,
}

impl Filter {
    fn matches(&self, event: &Event) -> bool {
        self.kind.is_none_or(|kind| kind == event.kind)
            && self
                .signals
                .as_ref()
                .is_none_or(|signals| signals.contains(&event.signal))
            && self.systems.as_ref().is_none_or(|systems| {
                systems.contains(&event.system) || systems.contains(&event.sender)
            })
            && self.entity.is_none_or(|x| event.mentions_entity(&x))
            && self.id.is_none_or(|id| event.mentions_id(id))
    }
}

type Cancels = BTreeMap<(u128, u64), u64>;

fn cancels_in(save: &TrackingSave) -> Cancels {
    let mut cancels = Cancels::new();
    for frame in save.frames() {
        for signal in &frame.signals {
            for change in signal.changes.iter().filter(|x| x.cancelled) {
                cancels
                    .entry((signal.signal, change.id))
                    .or_insert(frame.tick);
            }
        }
    }
    cancels
}

fn delivered_candidates<'a>(
    save: &'a TrackingSave,
    signal: u128,
    row: &ReceivedCount,
    tick: u64,
    cancels: &Cancels,
) -> Vec<&'a TrackedInstance> {
    let Some((sent, _)) = row
        .sent_tick
        .and_then(|sent_tick| save.sent_instances(sent_tick, signal))
    else {
        return Vec::new();
    };
    sent.iter()
        .filter(|instance| instance.sender == row.sender)
        .filter(|instance| match instance.window {
            None => !row.windowed,
            Some((start, end)) => start <= tick && tick < end,
        })
        .filter(|instance| {
            cancels
                .get(&(signal, instance.id))
                .is_none_or(|cancelled| *cancelled >= tick)
        })
        .collect()
}

fn instance_of<'a>(
    save: &'a TrackingSave,
    signal: &TrackedSignalFrame,
    system: u128,
    sender: u128,
    id: u64,
) -> Option<(u64, &'a TrackedInstance)> {
    signal
        .received
        .iter()
        .filter(|row| row.receiver == system && row.sender == sender)
        .filter_map(|row| row.sent_tick)
        .find_map(|sent_tick| {
            let (sent, _) = save.sent_instances(sent_tick, signal.signal)?;
            let instance = sent.iter().find(|x| x.id == id)?;
            Some((sent_tick, instance))
        })
}

fn visit<'a>(
    save: &'a TrackingSave,
    ticks: Option<&Range<u64>>,
    cancels: &Cancels,
    mut each: impl FnMut(Event<'a>),
) {
    let range = ticks.cloned().unwrap_or(0..u64::MAX);
    for frame in save.frames_in(range) {
        for signal in &frame.signals {
            for instance in &signal.instances {
                each(Event {
                    tick: frame.tick,
                    kind: Kind::Sent,
                    signal: signal.signal,
                    system: instance.sender,
                    sender: instance.sender,
                    sent_tick: Some(frame.tick),
                    windowed: instance.window.is_some(),
                    count: 1,
                    id: Some(instance.id),
                    instances: vec![instance],
                    payload: None,
                });
            }
            for row in signal.received.iter().filter(|row| row.count > 0) {
                each(Event {
                    tick: frame.tick,
                    kind: Kind::Read,
                    signal: signal.signal,
                    system: row.receiver,
                    sender: row.sender,
                    sent_tick: row.sent_tick,
                    windowed: row.windowed,
                    count: row.count,
                    id: None,
                    instances: delivered_candidates(save, signal.signal, row, frame.tick, cancels),
                    payload: None,
                });
            }
            for change in &signal.changes {
                let found = instance_of(save, signal, change.system, change.sender, change.id);
                each(Event {
                    tick: frame.tick,
                    kind: if change.cancelled {
                        Kind::Cancelled
                    } else {
                        Kind::Modified
                    },
                    signal: signal.signal,
                    system: change.system,
                    sender: change.sender,
                    sent_tick: found.map(|x| x.0),
                    windowed: found.is_some_and(|x| x.1.window.is_some()),
                    count: 1,
                    id: Some(change.id),
                    instances: found.map(|x| x.1).into_iter().collect(),
                    payload: change.payload.as_deref(),
                });
            }
        }
    }
}

fn unread_ids(save: &TrackingSave, cancels: &Cancels) -> BTreeSet<(u128, u64)> {
    let mut read_groups = BTreeSet::new();
    for frame in save.frames() {
        for signal in &frame.signals {
            for row in signal.received.iter().filter(|row| row.count > 0) {
                read_groups.insert((signal.signal, row.sender, row.sent_tick));
            }
        }
    }

    let mut unread = BTreeSet::new();
    visit(save, None, cancels, |event| {
        let group = (event.signal, event.sender, event.sent_tick);
        if event.kind == Kind::Sent && !read_groups.contains(&group) {
            let cancelled = event
                .id
                .is_some_and(|id| cancels.contains_key(&(event.signal, id)));
            if let (Some(id), false) = (event.id, cancelled) {
                unread.insert((event.signal, id));
            }
        }
    });
    unread
}

pub fn run_query(save: &TrackingSave, args: &QueryArgs) -> Result<String, String> {
    let signals = args
        .signal
        .as_ref()
        .map(|needle| resolve_names(signal_names(save), needle, "signal"))
        .transpose()?;
    let systems = args
        .system
        .as_ref()
        .map(|needle| resolve_names(system_names(save), needle, "system"))
        .transpose()?;
    let filter = Filter {
        kind: if args.unread {
            Some(Kind::Sent)
        } else {
            args.kind
        },
        signals,
        systems,
        entity: args.entity,
        id: args.id,
    };

    let cancels = cancels_in(save);
    let unread = args.unread.then(|| unread_ids(save, &cancels));
    let limit = match args.limit {
        Some(0) => usize::MAX,
        Some(limit) => limit,
        None => DEFAULT_LIMIT,
    };

    let mut lines = Vec::new();
    let mut matched = 0usize;
    visit(save, args.ticks.as_ref(), &cancels, |event| {
        let keep = filter.matches(&event)
            && unread.as_ref().is_none_or(|unread| {
                event
                    .id
                    .is_some_and(|id| unread.contains(&(event.signal, id)))
            });
        if keep {
            matched += 1;
            if lines.len() < limit {
                lines.push(event.line(save));
            }
        }
    });

    if matched > lines.len() {
        lines.push(format!(
            "... {} more, narrow the query or raise --limit",
            matched - lines.len()
        ));
    }
    if lines.is_empty() {
        lines.push("nothing matches".to_string());
    }
    Ok(lines.join("\n"))
}

#[derive(Default)]
struct SignalTotals {
    sent: u64,
    read: u64,
    cancelled: u64,
    modified: u64,
    omitted: u64,
}

pub fn report(save: &TrackingSave) -> String {
    let cancels = cancels_in(save);
    let unread = unread_ids(save, &cancels);
    let mut totals: BTreeMap<u128, SignalTotals> = BTreeMap::new();
    let mut active: BTreeSet<u128> = BTreeSet::new();
    let mut timing: BTreeMap<(u128, u128), (u64, u64)> = BTreeMap::new();
    let mut frames = 0;

    for frame in save.frames() {
        frames += 1;
        for signal in &frame.signals {
            let entry = totals.entry(signal.signal).or_default();
            entry.omitted += signal.instances_omitted;
            for sent in &signal.sent {
                entry.sent += sent.count;
                active.insert(sent.sender);
            }
            for row in &signal.received {
                entry.read += row.count;
                entry.cancelled += row.cancelled;
                entry.modified += row.modified;
                active.insert(row.receiver);
                if row.count == 0 || row.windowed {
                    continue;
                }
                let (same, late) = timing.entry((signal.signal, row.receiver)).or_default();
                if row.sent_tick == Some(frame.tick) {
                    *same += row.count;
                } else {
                    *late += row.count;
                }
            }
        }
    }

    let range = save.tick_range();
    let mut out = vec![format!(
        "recording: ticks {}..{}, {frames} ticks with activity, {} systems, {} signal types, memory limit {} MB",
        range.start,
        range.end,
        save.system_order().len(),
        save.signal_ids().count(),
        save.memory_limit() / (1024 * 1024)
    )];
    if !save.intervals().is_empty() {
        out.push(format!(
            "paused intervals: {} (not included)",
            save.intervals().len()
        ));
    }

    let mut unread_per_signal: BTreeMap<u128, u64> = BTreeMap::new();
    for (signal, _) in &unread {
        *unread_per_signal.entry(*signal).or_default() += 1;
    }
    out.push(String::new());
    out.push("signals: sent, read, cancelled, modified, unread".to_string());
    let mut names: Vec<(&str, &u128, &SignalTotals)> = totals
        .iter()
        .map(|(id, total)| (short_name(save.signal_name(*id)), id, total))
        .collect();
    names.sort_by_key(|entry| entry.0);
    for (name, id, total) in &names {
        let unread = unread_per_signal.get(*id).copied().unwrap_or(0);
        out.push(format!(
            "  {name}: {}, {}, {}, {}, {unread}",
            total.sent, total.read, total.cancelled, total.modified
        ));
        if total.omitted > 0 {
            out.push(format!("    {} sent signals not recorded", total.omitted));
        }
    }

    let never: Vec<&str> = names
        .iter()
        .filter(|(_, _, total)| total.sent > 0 && total.read == 0)
        .map(|entry| entry.0)
        .collect();
    if !never.is_empty() {
        out.push(String::new());
        out.push(format!("sent but never read: {}", never.join(", ")));
    }

    let mut late: Vec<String> = timing
        .iter()
        .filter(|(_, (_, late))| *late > 0)
        .map(|((signal, receiver), (same, late))| {
            let mixed = if *same > 0 { " (mixed timing)" } else { "" };
            format!(
                "  {} read by {}: {late} a tick late, {same} on the same tick{mixed}",
                short_name(save.signal_name(*signal)),
                sender_name(save, *receiver)
            )
        })
        .collect();
    late.sort();
    if !late.is_empty() {
        out.push(String::new());
        out.push("one-tick signals read after their send tick:".to_string());
        out.extend(late);
    }

    let idle: Vec<String> = save
        .system_order()
        .iter()
        .filter(|id| !active.contains(id))
        .map(|id| sender_name(save, *id))
        .collect();
    if !idle.is_empty() {
        out.push(String::new());
        out.push(format!(
            "systems with no signal activity: {}",
            idle.join(", ")
        ));
    }
    out.join("\n")
}

fn load(path: Option<&String>) -> Result<TrackingSave, String> {
    let path = path.ok_or_else(|| format!("give a recording file\n{QUERY_USAGE}"))?;
    Source::File(path.into())
        .open_file()?
        .ok_or_else(|| format!("`{path}` could not be read"))
}

pub fn command(args: &[String]) -> Option<Result<String, String>> {
    let name = args.first()?;
    let run = || -> Result<String, String> {
        let save = load(args.get(1))?;
        if name == "report" {
            return Ok(report(&save));
        }
        run_query(&save, &QueryArgs::parse(&args[2..])?)
    };
    (name == "report" || name == "query").then(run)
}

#[cfg(test)]
mod tests {
    use super::{EntityFilter, Event, Kind, QueryArgs, command, parse_ticks, report, run_query};
    use crate::test_support::recorded;
    use reacs::tracking::{TrackedInstance, TrackingSave};

    fn query(save: &TrackingSave, args: &[&str]) -> String {
        let args: Vec<String> = args.iter().map(|x| x.to_string()).collect();
        run_query(save, &QueryArgs::parse(&args).unwrap()).unwrap()
    }

    #[test]
    fn a_send_shows_id_sender_and_payload() {
        let save = recorded(1);
        assert_eq!(
            query(&save, &["--signal", "FixturePing", "--kind", "sent"]),
            "0\tsent\tFixturePing\tfixture_emit\t#0 FixturePing { _: 1 }\n\
             0\tsent\tFixturePing\tfixture_emit\t#1 FixturePing { _: 2 }"
        );
    }

    #[test]
    fn a_read_names_the_signals_it_got_and_their_origin() {
        let save = recorded(2);
        assert_eq!(
            query(
                &save,
                &["--signal", "FixturePing", "--kind", "read", "--tick", "1"]
            ),
            "1\tread\tFixturePing\tfixture_early\t#0,#1 count=2 from=fixture_emit@0"
        );
    }

    #[test]
    fn a_window_counts_as_read_only_while_it_is_running() {
        let save = recorded(4);
        assert_eq!(
            query(
                &save,
                &["--signal", "FixtureBeam", "--kind", "read", "--tick", "2"]
            ),
            "2\tread\tFixtureBeam\tfixture_beam_reader\t#0 count=1 from=fixture_emit@0 window"
        );
    }

    #[test]
    fn a_system_filter_finds_both_its_sends_and_reads_of_them() {
        let save = recorded(1);
        let lines = query(&save, &["--system", "fixture_emit", "--tick", "0"]);
        assert!(lines.contains("\tsent\t"));
        assert!(lines.contains("\tread\tFixturePing\tfixture_late"));
    }

    #[test]
    fn output_is_capped_and_says_how_much_is_left() {
        let save = recorded(4);
        let lines = query(&save, &["--limit", "1"]);
        assert_eq!(lines.lines().count(), 2);
        assert!(lines.lines().last().unwrap().contains("more"));
        assert!(!query(&save, &["--limit", "0"]).contains("more,"));
    }

    #[test]
    fn nothing_found_is_said_plainly() {
        assert_eq!(query(&recorded(1), &["--tick", "900"]), "nothing matches");
    }

    #[test]
    fn an_unknown_name_lists_what_exists() {
        let save = recorded(1);
        let args = QueryArgs::parse(&["--signal".to_string(), "Nope".to_string()]).unwrap();
        let error = run_query(&save, &args).err().unwrap();
        assert!(error.contains("no signal matches `Nope`"));
        assert!(error.contains("FixturePing"));
    }

    #[test]
    fn an_exact_name_beats_a_partial_one() {
        let save = recorded(1);
        let lines = query(&save, &["--signal", "FixturePing"]);
        assert!(lines.contains("FixturePing"));
        assert!(!lines.contains("FixtureBeam"));
    }

    #[test]
    fn partial_names_work_when_nothing_is_exact() {
        let save = recorded(1);
        assert!(query(&save, &["--signal", "Ping"]).contains("FixturePing"));
        assert!(query(&save, &["--system", "emit"]).contains("fixture_emit"));
    }

    #[test]
    fn unread_finds_signals_nobody_received() {
        let save = recorded(1);
        let unread = query(&save, &["--unread"]);
        assert!(unread.contains("FixtureBeam"), "{unread}");
        assert!(!unread.contains("FixturePing"), "{unread}");
    }

    #[test]
    fn ticks_parse_as_one_or_a_range() {
        assert_eq!(parse_ticks("5"), Some(5..6));
        assert_eq!(parse_ticks("5..9"), Some(5..9));
        assert_eq!(parse_ticks("..9"), Some(0..9));
        assert_eq!(parse_ticks("5.."), Some(5..u64::MAX));
        assert_eq!(parse_ticks("nope"), None);
    }

    #[test]
    fn entities_match_by_index_or_exactly() {
        let bits = (2u64 << 32) | 50;
        assert_eq!(EntityFilter::parse("50v2"), Some(EntityFilter::Exact(bits)));
        assert!(EntityFilter::parse("50v2").unwrap().matches(bits));
        assert!(!EntityFilter::parse("50v3").unwrap().matches(bits));
        assert!(EntityFilter::parse("50").unwrap().matches(bits));
        assert!(!EntityFilter::parse("51").unwrap().matches(bits));
        assert_eq!(EntityFilter::parse("v"), None);
    }

    fn event<'a>(kind: Kind, instances: Vec<&'a TrackedInstance>) -> Event<'a> {
        Event {
            tick: 9,
            kind,
            signal: 1,
            system: 2,
            sender: 3,
            sent_tick: Some(7),
            windowed: false,
            count: 2,
            id: Some(4),
            instances,
            payload: Some("Wave { power: 7 }"),
        }
    }

    fn instance(id: u64, entity: u64) -> TrackedInstance {
        TrackedInstance {
            id,
            sender: 3,
            window: None,
            entities: vec![entity],
            payload: "Hit".to_string(),
        }
    }

    #[test]
    fn a_read_with_fewer_than_listed_says_so() {
        let save = recorded(1);
        let (a, b, c) = (instance(1, 0), instance(2, 0), instance(3, 0));
        let line = event(Kind::Read, vec![&a, &b, &c]).line(&save);
        assert!(line.contains("#1,#2,#3 count=2 of=3"), "{line}");
    }

    #[test]
    fn a_modify_shows_the_new_value_and_a_cancel_the_id() {
        let save = recorded(1);
        assert!(
            event(Kind::Modified, Vec::new())
                .line(&save)
                .contains("#4 now=Wave { power: 7 }")
        );
        assert!(
            event(Kind::Cancelled, Vec::new())
                .line(&save)
                .contains("#4 from=")
        );
    }

    #[test]
    fn an_event_mentions_its_entities_and_ids() {
        let a = instance(1, 77);
        let found = event(Kind::Read, vec![&a]);
        assert!(found.mentions_entity(&EntityFilter::Exact(77)));
        assert!(!found.mentions_entity(&EntityFilter::Exact(78)));
        assert!(found.mentions_id(1));
        assert!(found.mentions_id(4));
        assert!(!found.mentions_id(5));
    }

    #[test]
    fn the_report_summarizes_the_recording() {
        let text = report(&recorded(4));
        assert!(text.starts_with("recording: ticks 0..4"), "{text}");
        assert!(text.contains("FixturePing: 6,"), "{text}");
        assert!(text.contains("read by fixture_early"), "{text}");
    }

    #[test]
    fn other_arguments_are_left_for_the_window() {
        assert!(command(&["run.bin".to_string()]).is_none());
        assert!(command(&["--port".to_string(), "1".to_string()]).is_none());
        assert!(command(&[]).is_none());
    }

    #[test]
    fn a_missing_file_is_an_error() {
        let result = command(&["report".to_string(), "/no/such/file".to_string()]);
        assert!(result.unwrap().is_err());
    }
}
