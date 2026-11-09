use crate::graph::{NodeKey, StubKey, WireKey, counted, sender_name, short_name};
use reacs::tracking::{TrackedFrame, TrackingSave, UNKNOWN_SENDER};
use std::collections::BTreeMap;
use std::ops::Range;

pub struct Link {
    pub signal: u128,
    pub text: String,
    pub target: Option<NodeKey>,
}

pub struct NodeLinks {
    pub inputs: Vec<Link>,
    pub outputs: Vec<Link>,
}

struct Reads {
    first: u64,
    first_count: u64,
    first_cancelled: u64,
    first_modified: u64,
    last: u64,
    ticks: u64,
}

fn frame_at<'a>(frames: &[&'a TrackedFrame], tick: u64) -> Option<&'a TrackedFrame> {
    frames
        .binary_search_by_key(&tick, |frame| frame.tick)
        .ok()
        .map(|index| frames[index])
}

fn inputs_of(save: &TrackingSave, frames: &[&TrackedFrame], node: NodeKey) -> Vec<Link> {
    let (tick, system) = node;
    let Some(frame) = frame_at(frames, tick) else {
        return Vec::new();
    };

    let mut links = Vec::new();
    for signal in &frame.signals {
        let name = short_name(save.signal_name(signal.signal));
        for received in signal.received.iter().filter(|x| x.receiver == system) {
            let sender = sender_name(save, received.sender);
            let recorded = received.sent_tick.filter(|sent| {
                received.sender != UNKNOWN_SENDER && frame_at(frames, *sent).is_some()
            });
            let origin = match (received.sent_tick, recorded) {
                (Some(sent), Some(_)) => format!("tick {sent}"),
                (Some(sent), None) => format!("tick {sent}, no longer recorded"),
                (None, _) => "sent before recording".to_string(),
            };
            links.push(Link {
                signal: signal.signal,
                text: format!(
                    "{} from {sender}, {origin}",
                    counted(name, received.count, received.cancelled, received.modified)
                ),
                target: recorded.map(|sent| (sent, received.sender)),
            });
        }
    }
    links
}

fn reads_of(frames: &[&TrackedFrame], node: NodeKey) -> BTreeMap<(u128, u128), Reads> {
    let (tick, system) = node;
    let start = frames.partition_point(|frame| frame.tick < tick);
    let mut reads = BTreeMap::new();
    for frame in &frames[start..] {
        for signal in &frame.signals {
            let own = |x: &&reacs::tracking::ReceivedCount| {
                x.sender == system && x.sent_tick == Some(tick)
            };
            for received in signal.received.iter().filter(own) {
                let entry = reads
                    .entry((signal.signal, received.receiver))
                    .or_insert(Reads {
                        first: frame.tick,
                        first_count: received.count,
                        first_cancelled: received.cancelled,
                        first_modified: received.modified,
                        last: frame.tick,
                        ticks: 1,
                    });
                if entry.last != frame.tick {
                    entry.last = frame.tick;
                    entry.ticks += 1;
                }
            }
        }
    }
    reads
}

fn outputs_of(save: &TrackingSave, frames: &[&TrackedFrame], node: NodeKey) -> Vec<Link> {
    let (tick, system) = node;
    let Some(frame) = frame_at(frames, tick) else {
        return Vec::new();
    };

    let reads = reads_of(frames, node);
    let mut found: Vec<(u64, Link)> = Vec::new();
    for signal in &frame.signals {
        let name = short_name(save.signal_name(signal.signal));
        for sent in signal.sent.iter().filter(|x| x.sender == system) {
            let readers = reads
                .iter()
                .filter(|((id, _), _)| *id == signal.signal)
                .collect::<Vec<_>>();
            if readers.is_empty() {
                found.push((
                    u64::MAX,
                    Link {
                        signal: signal.signal,
                        text: format!("{name} ({}): nobody read it", sent.count),
                        target: None,
                    },
                ));
            }
            for ((_, receiver), read) in readers {
                let more = match read.ticks {
                    1 => String::new(),
                    ticks => format!(", and {} more ticks", ticks - 1),
                };
                let receiver_name = sender_name(save, *receiver);
                found.push((
                    read.first,
                    Link {
                        signal: signal.signal,
                        text: format!(
                            "{} to {receiver_name}, tick {}{more}",
                            counted(
                                name,
                                read.first_count,
                                read.first_cancelled,
                                read.first_modified
                            ),
                            read.first
                        ),
                        target: Some((read.first, *receiver)),
                    },
                ));
            }
        }
    }
    found.sort_by_key(|(first, _)| *first);
    found.into_iter().map(|(_, link)| link).collect()
}

pub fn links_of(save: &TrackingSave, frames: &[&TrackedFrame], node: NodeKey) -> NodeLinks {
    NodeLinks {
        inputs: inputs_of(save, frames, node),
        outputs: outputs_of(save, frames, node),
    }
}

pub fn links_of_stub(
    save: &TrackingSave,
    frames: &[&TrackedFrame],
    stub: StubKey,
    shown: &Range<u64>,
) -> NodeLinks {
    let links = links_of(save, frames, stub.node);
    let (mut kept, other) = if stub.input {
        (links.inputs, links.outputs)
    } else {
        (links.outputs, links.inputs)
    };
    drop(other);

    kept.retain(|link| link.signal == stub.signal);
    let off_screen = |link: &Link| link.target.is_none_or(|(tick, _)| !shown.contains(&tick));
    if kept.iter().any(off_screen) {
        kept.retain(off_screen);
    }
    if stub.input {
        NodeLinks {
            inputs: kept,
            outputs: Vec::new(),
        }
    } else {
        NodeLinks {
            inputs: Vec::new(),
            outputs: kept,
        }
    }
}

pub fn links_of_wire(save: &TrackingSave, wire: WireKey) -> NodeLinks {
    let name = short_name(save.signal_name(wire.signal));
    let link = |word: &str, (tick, system): NodeKey| Link {
        signal: wire.signal,
        text: format!("{name} {word} {}, tick {tick}", sender_name(save, system)),
        target: Some((tick, system)),
    };
    NodeLinks {
        inputs: vec![link("from", wire.from)],
        outputs: vec![link("to", wire.to)],
    }
}

#[cfg(test)]
mod tests {
    use super::{Link, links_of, links_of_stub, links_of_wire};
    use crate::graph::short_name;
    use crate::graph::{StubKey, WireKey};
    use crate::test_support::recorded;
    use reacs::tracking::TrackingSave;
    use std::ops::Range;

    fn id(save: &TrackingSave, name: &str) -> u128 {
        *save
            .system_order()
            .iter()
            .find(|id| short_name(save.system_name(**id)) == name)
            .unwrap()
    }

    fn find<'a>(links: &'a [Link], text: &str) -> &'a Link {
        links
            .iter()
            .find(|link| link.text.contains(text))
            .unwrap_or_else(|| panic!("no link with {text}"))
    }

    fn frames(save: &TrackingSave, ticks: Range<u64>) -> Vec<&reacs::tracking::TrackedFrame> {
        save.frames_in(ticks).collect()
    }

    #[test]
    fn an_input_jumps_back_to_its_sender_on_an_earlier_tick() {
        let save = recorded(4);
        let links = links_of(&save, &frames(&save, 0..4), (1, id(&save, "fixture_early")));

        let ping = find(&links.inputs, "FixturePing (2) from fixture_emit, tick 0");
        assert_eq!(ping.target, Some((0, id(&save, "fixture_emit"))));
    }

    #[test]
    fn an_input_whose_sender_left_the_recording_cannot_be_followed() {
        let save = recorded(4);
        let links = links_of(&save, &frames(&save, 1..4), (1, id(&save, "fixture_early")));

        let ping = find(&links.inputs, "no longer recorded");
        assert_eq!(ping.target, None);
    }

    #[test]
    fn an_output_jumps_forward_to_each_reader_including_later_ticks() {
        let save = recorded(4);
        let emit = id(&save, "fixture_emit");
        let links = links_of(&save, &frames(&save, 0..4), (0, emit));

        let same_tick = find(&links.outputs, "FixturePing (2) to fixture_late, tick 0");
        assert_eq!(same_tick.target, Some((0, id(&save, "fixture_late"))));
        let next_tick = find(&links.outputs, "FixturePing (2) to fixture_early, tick 1");
        assert_eq!(next_tick.target, Some((1, id(&save, "fixture_early"))));

        let beam = find(&links.outputs, "FixtureBeam");
        assert_eq!(beam.target.unwrap().1, id(&save, "fixture_beam_reader"));
    }

    #[test]
    fn a_window_read_over_many_ticks_is_one_link_to_its_first_read() {
        let save = recorded(4);
        let emit = id(&save, "fixture_emit");
        let reader = id(&save, "fixture_beam_reader");
        let links = links_of(&save, &frames(&save, 0..4), (0, emit));

        let beams: Vec<&Link> = links
            .outputs
            .iter()
            .filter(|link| link.text.contains("FixtureBeam"))
            .collect();
        assert_eq!(beams.len(), 1);
        assert_eq!(beams[0].target.unwrap().1, reader);
    }

    #[test]
    fn an_output_nobody_read_is_listed_but_cannot_be_followed() {
        let save = recorded(4);
        let links = links_of(&save, &frames(&save, 0..1), (0, id(&save, "fixture_emit")));

        let beam = find(&links.outputs, "FixtureBeam");
        assert!(beam.text.contains("nobody read it"));
        assert_eq!(beam.target, None);
    }

    #[test]
    fn a_wire_links_to_its_sender_and_its_receiver() {
        let save = recorded(4);
        let (emit, early) = (id(&save, "fixture_emit"), id(&save, "fixture_early"));
        let signal = save.frames_in(1..2).next().unwrap().signals[0].signal;
        let links = links_of_wire(
            &save,
            WireKey {
                from: (0, emit),
                to: (1, early),
                signal,
                windowed: false,
            },
        );

        assert_eq!(
            links.inputs[0].text,
            "FixturePing from fixture_emit, tick 0"
        );
        assert_eq!(links.inputs[0].target, Some((0, emit)));
        assert_eq!(
            links.outputs[0].text,
            "FixturePing to fixture_early, tick 1"
        );
        assert_eq!(links.outputs[0].target, Some((1, early)));
    }

    #[test]
    fn an_off_screen_edge_only_links_to_what_is_off_screen() {
        let save = recorded(4);
        let early = id(&save, "fixture_early");
        let signal = save.frames_in(1..2).next().unwrap().signals[0].signal;
        let stub = StubKey {
            node: (1, early),
            signal,
            input: true,
        };

        let links = links_of_stub(&save, &frames(&save, 0..4), stub, &(1..2));
        assert!(links.outputs.is_empty());
        assert_eq!(links.inputs.len(), 1);
        assert_eq!(links.inputs[0].target.unwrap().0, 0);
    }

    #[test]
    fn an_edge_whose_first_read_is_on_screen_still_offers_it() {
        let save = recorded(4);
        let emit = id(&save, "fixture_emit");
        let beam = save
            .frames_in(0..1)
            .next()
            .unwrap()
            .signals
            .iter()
            .find(|x| save.signal_name(x.signal).ends_with("FixtureBeam"))
            .unwrap()
            .signal;
        let stub = StubKey {
            node: (0, emit),
            signal: beam,
            input: false,
        };

        let links = links_of_stub(&save, &frames(&save, 0..4), stub, &(0..4));
        assert_eq!(links.outputs.len(), 1);
    }

    #[test]
    fn readers_come_in_the_order_they_read() {
        let save = recorded(4);
        let links = links_of(&save, &frames(&save, 0..4), (0, id(&save, "fixture_emit")));

        let ticks: Vec<u64> = links
            .outputs
            .iter()
            .filter_map(|link| link.target.map(|target| target.0))
            .collect();
        assert!(ticks.windows(2).all(|pair| pair[0] <= pair[1]));
    }
}
