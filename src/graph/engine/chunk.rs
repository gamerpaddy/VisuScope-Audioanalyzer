//! One chunk of the routing graph: every node in order, then the outputs.

use super::*;

impl Run {
    /// Process `n` frames through every node in order.
    pub(super) fn chunk(&mut self, n: usize, clocked: bool) {
        let sh = self.e.sh.clone();
        let rate = self.rate;
        let safety = crate::prof::safety_ms() as usize;
        let prime = rate as usize * (25 + safety) / 1000;
        let probe = *self.e.probe.lock();
        let probe_at = probe.and_then(|p| self.nodes.iter().position(|n| n.id == p.node).map(|i| (i, p)));
        // the probe plays on a device of its own: only the output nodes that play to that very device are silent meanwhile
        // (two streams on one device would play both), every other output goes on
        let mute_dev: Option<String> = (probe_at.is_some() && self.e.probe_solo.load(Relaxed)).then(|| match bus().probe_dev.lock().as_str() {
            "" => bus().first_out.lock().clone(),
            PROBE_DEFAULT => String::new(),
            d => d.to_string(),
        });
        if !self.mods.is_empty() {
            // the level of each followed signal over the block before (the signal is a block ahead of its parameter at most)
            let e = self.e.clone();
            for rm in self.mods.iter_mut() {
                let b = &self.nodes[rm.src].out;
                let len = n.min(b[0].len());
                let mean = if len == 0 { 0.0 } else { (0..len).map(|k| 0.5 * (b[0][k] + b[1][k])).sum::<f32>() / len as f32 };
                let v = if rm.sw {
                    // a switch: on above the upper threshold, off below the lower one, in between it stays as it was
                    let (lo, hi) = (rm.m.in_lo.min(rm.m.in_hi), rm.m.in_lo.max(rm.m.in_hi));
                    let on = if mean >= hi && mean.is_finite() { true } else if mean <= lo { false } else { rm.on.unwrap_or(mean >= 0.5 * (lo + hi)) };
                    rm.on = Some(on);
                    if on { rm.m.out_hi } else { rm.m.out_lo }
                } else {
                    rm.m.map(mean)
                };
                if rm.last.is_nan() || (v - rm.last).abs() > 1e-6 * (rm.m.out_hi - rm.m.out_lo).abs().max(1e-6) {
                    rm.last = v;
                    if let Some(slot) = rm.node.p.get_mut(rm.m.param) {
                        *slot = v;
                    }
                    Self::apply_with(rate, &e, &mut self.nodes[rm.dst], &rm.node);
                }
            }
            let mut live = MOD_LIVE.lock();
            live.clear();
            live.extend(self.mods.iter().map(|r| (r.m.node, r.m.param, r.last)));
        }
        for i in 0..self.nodes.len() {
            let (before, rest) = self.nodes.split_at_mut(i);
            let node = &mut rest[0];
            let is_master = clocked && Some(i) == self.master;
            let inp = node.src.first().copied().flatten().map(|(s, p)| outbuf(&before[s], p));
            let [o0, o1] = &mut node.out;
            match &mut node.rt {
                Rt::Input { rs, gain, delay, primed, .. } => {
                    let got = match rs {
                        Some(rs) => pull(rs, primed, prime, is_master, &mut o0[..n], &mut o1[..n]),
                        None => 0,
                    };
                    o0[got..n].fill(0.0);
                    o1[got..n].fill(0.0);
                    crate::lattest::mark(3, o0[..n].iter().chain(o1[..n].iter()).any(|v| v.abs() > 0.01));
                    delay.process(&mut o0[..n], &mut o1[..n]);
                    let (g0, dg) = span(&mut node.cur[0], *gain, n);
                    if (g0 - 1.0).abs() > 1e-6 || dg != 0.0 {
                        for k in 0..n {
                            let g = g0 + dg * k as f32;
                            o0[k] *= g;
                            o1[k] *= g;
                        }
                    }
                }
                Rt::Gen { st, idx } => {
                    match self.gens.get(*idx) {
                        Some(p) => {
                            st.render(p, &mut self.gen_buf[..n * 2], 2);
                            for k in 0..n {
                                o0[k] = self.gen_buf[2 * k];
                                o1[k] = self.gen_buf[2 * k + 1];
                            }
                            crate::lattest::mark(0, o0[..n].iter().any(|v| v.abs() > 1e-4));
                        }
                        None => {
                            o0[..n].fill(0.0);
                            o1[..n].fill(0.0);
                        }
                    }
                }
                Rt::Tl { rs, primed, .. } => {
                    let got = match rs {
                        Some(rs) => pull(rs, primed, prime, false, &mut o0[..n], &mut o1[..n]),
                        None => 0,
                    };
                    o0[got..n].fill(0.0);
                    o1[got..n].fill(0.0);
                }
                Rt::Mix { gain, mute, master } => {
                    o0[..n].fill(0.0);
                    o1[..n].fill(0.0);
                    for (p, s) in node.src.iter().enumerate() {
                        let Some((s, sp)) = s else { continue };
                        let pi = p.min(MIX_MAX - 1);
                        // (a mute or a change of the level fades over the block)
                        let tgt = if mute[pi] { 0.0 } else { gain[pi] * *master };
                        let (g0, dg) = span(&mut node.cur[pi], tgt, n);
                        if g0 == 0.0 && dg == 0.0 {
                            continue;
                        }
                        let src = outbuf(&before[*s], *sp);
                        for k in 0..n {
                            let g = g0 + dg * k as f32;
                            o0[k] += src[0][k] * g;
                            o1[k] += src[1][k] * g;
                        }
                    }
                }
                Rt::Split => {
                    // out 1: the left channel on both sides, out 2: the right channel on both sides
                    match inp {
                        Some(b) => {
                            o0[..n].copy_from_slice(&b[0][..n]);
                            o1[..n].copy_from_slice(&b[0][..n]);
                            node.out2[0][..n].copy_from_slice(&b[1][..n]);
                            node.out2[1][..n].copy_from_slice(&b[1][..n]);
                        }
                        None => {
                            o0[..n].fill(0.0);
                            o1[..n].fill(0.0);
                            node.out2[0][..n].fill(0.0);
                            node.out2[1][..n].fill(0.0);
                        }
                    }
                }
                Rt::Dual => {
                    // two stereo lines, both a copy of the input
                    match inp {
                        Some(b) => {
                            o0[..n].copy_from_slice(&b[0][..n]);
                            o1[..n].copy_from_slice(&b[1][..n]);
                            node.out2[0][..n].copy_from_slice(&b[0][..n]);
                            node.out2[1][..n].copy_from_slice(&b[1][..n]);
                        }
                        None => {
                            o0[..n].fill(0.0);
                            o1[..n].fill(0.0);
                            node.out2[0][..n].fill(0.0);
                            node.out2[1][..n].fill(0.0);
                        }
                    }
                }
                Rt::Merge => {
                    // each input is reduced to mono: the first becomes the left channel, the second the right
                    for (k, dst) in [&mut *o0, &mut *o1].into_iter().enumerate() {
                        match node.src.get(k).copied().flatten().map(|(s, p)| outbuf(&before[s], p)) {
                            Some(b) => (0..n).for_each(|j| dst[j] = 0.5 * (b[0][j] + b[1][j])),
                            None => dst[..n].fill(0.0),
                        }
                    }
                }
                Rt::Out { q, gain, mute, band, buf, aa, aa_for, dev, .. } => {
                    let mute_outputs = mute_dev.as_deref().is_some_and(|d| d == dev.as_str());
                    // scratch buffers of the node: nothing is allocated per block
                    let [bl, br] = buf;
                    let separate = node.src.len() == 2;
                    let port = |k: usize| node.src.get(k).copied().flatten().map(|(s, sp)| outbuf(&before[s], sp));
                    let have = gather_out(inp, port(0), port(1), separate, n, bl, br);
                    node.peak = peak2(&bl[..n], &br[..n], node.peak);
                    if have {
                        // (muted: the stream goes on with silence, so that the sound output does not run dry and mistake it for a gap)
                        let tgt = if *mute || mute_outputs { 0.0 } else { *gain };
                        let (g0, dg) = span(&mut node.cur[0], tgt, n);
                        for k in 0..n {
                            let g = g0 + dg * k as f32;
                            bl[k] *= g;
                            br[k] *= g;
                        }
                        // a device of a lower rate than the graph: what is above its Nyquist frequency would fold back into the sound
                        // (49 kHz played on a 48 kHz device is heard as 1 kHz), so it is filtered first
                        let osr = q.lock().out_sr;
                        if osr > 0 && osr < rate {
                            if *aa_for != osr {
                                *aa_for = osr;
                                aa.design(rate as f32, osr as f32);
                            }
                            aa.process(&mut bl[..n], &mut br[..n]);
                        }
                        band.process(&mut bl[..n], &mut br[..n], rate as f32);
                        let mut q = q.lock();
                        q.src_sr = rate;
                        q.push(&bl[..n], &br[..n]);
                    }
                    continue;
                }
                Rt::Rec { id } => {
                    let (a, b) = inp.map(|b| (&b[0][..n], &b[1][..n])).unwrap_or((&ZERO[..n], &ZERO[..n]));
                    if let Some(c) = bus().virt.lock().get(id.as_str()) {
                        c.feed(a, b);
                    }
                    node.peak = peak2(a, b, node.peak);
                    continue;
                }
                Rt::None if node.kind == Kind::Analyzer => {
                    let (a, b) = inp.map(|b| (&b[0][..n], &b[1][..n])).unwrap_or((&ZERO[..n], &ZERO[..n]));
                    crate::lattest::mark(4, a.iter().chain(b.iter()).any(|v| v.abs() > 0.01));
                    if sh.ring_owner.load(Relaxed) == 3 {
                        let mut ring = sh.ring.lock();
                        for k in 0..n {
                            ring.push(a[k], b[k]);
                        }
                        drop(ring);
                        sh.last_data_ms.store(sh.t0.elapsed().as_millis() as u64, Relaxed);
                        sh.feeding_silence.store(false, Relaxed);
                        sh.cv.notify_one();
                    }
                    node.peak = peak2(a, b, node.peak);
                    continue;
                }
                // effects: copy the input (silence when unconnected), then process in place
                rt => {
                    match inp {
                        Some(b) => {
                            o0[..n].copy_from_slice(&b[0][..n]);
                            o1[..n].copy_from_slice(&b[1][..n]);
                        }
                        None => {
                            o0[..n].fill(0.0);
                            o1[..n].fill(0.0);
                        }
                    }
                    if !node.bypass {
                        let (l, r) = (&mut o0[..n], &mut o1[..n]);
                        match rt {
                            Rt::Gain(g) => {
                                let (g0, dg) = span(&mut node.cur[0], *g, n);
                                for k in 0..n {
                                    let g = g0 + dg * k as f32;
                                    l[k] *= g;
                                    r[k] *= g;
                                }
                            }
                            Rt::Width { w, b } => stereo_width(l, r, *w, *b),
                            Rt::Util(m) => utility(l, r, *m),
                            Rt::Eq(u) => u.process(l, r),
                            Rt::Filter(u) => u.process(l, r),
                            Rt::Comp(u) => {
                                u.process(l, r);
                                node.extra = u.gr_db;
                            }
                            Rt::Lim(u) => {
                                u.process(l, r);
                                node.extra = u.gr_db;
                            }
                            Rt::Gate(u) => {
                                u.process(l, r);
                                node.extra = if u.open { 1.0 } else { 0.0 };
                            }
                            Rt::Dly(u) => u.process(l, r),
                            Rt::Dist(u) => u.process(l, r),
                            Rt::Phase(u) => u.process(l, r),
                            Rt::Shift(u) => u.process(l, r),
                            Rt::Conv { u, .. } => u.process(l, r),
                            Rt::Nr(u) => u.process(l, r),
                            Rt::Fuzz(u) => u.process(l, r),
                            Rt::Crush(u) => u.process(l, r),
                            Rt::Jit(u) => u.process(l, r),
                            Rt::Trem(u) => u.process(l, r),
                            Rt::Smooth(u) => u.process(l, r),
                            _ => {}
                        }
                    }
                }
            }
            node.peak = peak2(&o0[..n], &o1[..n], node.peak);
        }
        if let Some((i, p)) = probe_at {
            let node = &self.nodes[i];
            let src: Option<&[Vec<f32>; 2]> = if p.input { node.src.get(p.port as usize).copied().flatten().map(|(s, sp)| outbuf(&self.nodes[s], sp)) } else if node.kind.info().outputs > 0 { Some(outbuf(node, p.port)) } else { None };
            let [pl, pr] = &mut self.probe_buf;
            match src {
                Some(b) => {
                    pl[..n].copy_from_slice(&b[0][..n]);
                    pr[..n].copy_from_slice(&b[1][..n]);
                }
                None => {
                    pl[..n].fill(0.0);
                    pr[..n].fill(0.0);
                }
            }
            let mut q = self.probe_q.lock();
            q.src_sr = rate;
            q.push(&pl[..n], &pr[..n]);
        }
    }
}
