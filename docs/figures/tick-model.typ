// Adaptive client tick-period model and figures.
#import "@preview/lilaq:0.6.0" as lq

// Defaults checked against deform_{quic,foc}/src/{client,lib}.rs on 2026-09-16.
#let params = (
  base-frame-time: 1000 / 60, // milliseconds, for the illustrative 60 Hz game
  max-speedup: 0.10,
  max-slowdown: 0.05,
  buffer-target: 0,
  jitter-slack: 0.5, // QUIC; FOC uses the separate margin below
  slowdown-deadzone: 1,
  speedup-softness: 1,
  slowdown-softness: 3,
)
#let foc-jitter-slack = 2
#let buffer-fall = 0.60
#let buffer-rise = 0.05
#let panic-kick = 1
#let panic-max = 3
#let panic-decay = 0.955

// Effective setpoint T: the start of the nominal-rate region.
// The panic term sits here, so a rollback shifts the deadzone along with it.
#let setpoint = params.buffer-target + params.jitter-slack

// Rate multiplier r(x): ticks per second relative to nominal. `buffered` is the
// estimated number of inputs sitting in the server's buffer for future ticks
// (b_est in the spec, not the raw sample), in ticks.
#let rate-multiplier(buffered, panic: 0, slack: params.jitter-slack) = {
  let target = params.buffer-target + slack + panic
  (
    1 + params.max-speedup * calc.tanh(
      calc.max(target - buffered, 0) / params.speedup-softness
    ) - params.max-slowdown * calc.tanh(
      calc.max(buffered - target - params.slowdown-deadzone, 0) / params.slowdown-softness
    )
  )
}

// Target interval between simulation deadlines, in milliseconds.
#let tick-period(buffered, panic: 0, slack: params.jitter-slack) = (
  params.base-frame-time / rate-multiplier(buffered, panic: panic, slack: slack)
)

#let panic-after(samples, initial: panic-kick) = initial * calc.pow(panic-decay, samples)

// Illustrative feedback sequence: decay at each report, then kick if it
// reveals a local-input mismatch (the QUIC event order).
#let panic-events = (5, 10, 15, 20, 25)
#let panic-series = {
  let points = ((0, 0),)
  let kicks = ()
  let margin = 0
  for sample in range(1, 101) {
    points.push((sample, margin))
    margin *= panic-decay
    points.push((sample, margin))
    if sample in panic-events {
      margin = calc.min(margin + panic-kick, panic-max)
      points.push((sample, margin))
      kicks.push((sample, margin))
    }
  }
  (points: points, kicks: kicks)
}

// The model as a display equation.
#let equation = $
  T &= g + s \
  r(x) &= 1 + u dot tanh(max(T - x, 0) / a) - d dot tanh(max(x - T - z, 0) / c) \
  y(x) &= Delta / r(x)
$

// Vector plots evaluated from the same functions as the displayed equations.
#let blue = rgb("215c91")
#let orange = rgb("c56820")
#let purple = rgb("774899")
#let guide = (paint: luma(150), thickness: 0.6pt, dash: "dashed")

#let period-plot() = {
  set text(size: 9pt)
  let xs = lq.linspace(0, 8, num: 401)
  lq.diagram(
    width: 13.5cm, height: 6.2cm,
    xlim: (0, 8), ylim: (15, 17.7),
    xlabel: [Estimated buffered inputs $x$ (ticks)],
    ylabel: [Tick period $y(x)$ (ms)],
    xaxis: (ticks: range(0, 9)),
    yaxis: (ticks: (15, 15.5, 16, 16.5, 17, 17.5)),
    legend: (position: bottom + right),
    lq.hlines(params.base-frame-time, stroke: guide),
    lq.plot(xs, xs.map(x => tick-period(x)), mark: none,
      stroke: blue + 1.5pt, label: [QUIC: $s = #params.jitter-slack$]),
    lq.plot(xs, xs.map(x => tick-period(x, slack: foc-jitter-slack)), mark: none,
      stroke: (paint: orange, thickness: 1.5pt, dash: "dashed"),
      label: [FOC: $s = #foc-jitter-slack$]),
    lq.place(7.8, params.base-frame-time, align: bottom + right,
      pad(bottom: 4pt)[$Delta$]),
  )
}

#let panic-plot() = {
  set text(size: 8.5pt)
  let xs = lq.linspace(0, 9, num: 361)
  let reports = range(0, 81)
  grid(
    columns: (1fr, 1fr), column-gutter: 10pt,
    [
      #align(center)[*(a) Raising the target*]
      #lq.diagram(
        width: 6.5cm, height: 5.4cm,
        xlim: (0, 9), ylim: (15, 17.7),
        xlabel: [$x$ (ticks)], ylabel: [$y(x)$ (ms)],
        xaxis: (ticks: (0, 3, 6, 9)),
        yaxis: (ticks: (15, 16, 17)),
        legend: (position: bottom + right),
        lq.hlines(params.base-frame-time, stroke: guide),
        ..(0, panic-kick, panic-max).enumerate().map(((i, p)) => lq.plot(
          xs, xs.map(x => tick-period(x, panic: p, slack: foc-jitter-slack)),
          mark: none, stroke: (paint: (blue, orange, purple).at(i), thickness: 1.3pt,
            dash: ("solid", "dashed", "dotted").at(i)), label: [$P = #p$],
        )),
      )
    ],
    [
      #align(center)[*(b) Relaxing after one increase*]
      #lq.diagram(
        width: 6.5cm, height: 5.4cm,
        xlim: (-5, 80), ylim: (15, 17.7),
        xlabel: [Feedback samples since increase], ylabel: [$y(x)$ (ms)],
        xaxis: (ticks: (0, 20, 40, 60, 80)),
        yaxis: (ticks: (15, 16, 17)),
        lq.hlines(params.base-frame-time, stroke: guide),
        lq.plot((-5, 0, 0),
          (params.base-frame-time, params.base-frame-time,
            tick-period(foc-jitter-slack, panic: panic-kick, slack: foc-jitter-slack)),
          mark: none, stroke: orange + 1.5pt),
        lq.plot(reports, reports.map(n => tick-period(
          foc-jitter-slack, panic: panic-after(n), slack: foc-jitter-slack,
        )), mark: none, stroke: orange + 1.5pt),
        lq.place(75, params.base-frame-time, align: bottom + right,
          pad(bottom: 4pt)[$Delta$]),
      )
    ],
  )
}

#let panic-history-plot() = {
  set text(size: 9pt)
  lq.diagram(
    width: 13.5cm, height: 5.8cm,
    xlim: (0, 100), ylim: (0, panic-max + 0.4),
    xlabel: [Received feedback samples],
    ylabel: [Panic margin $P$ (ticks)],
    xaxis: (ticks: (0, 20, 40, 60, 80, 100)),
    yaxis: (ticks: range(0, panic-max + 1)),
    legend: (position: top + right),
    lq.hlines(panic-max, stroke: guide),
    lq.plot(panic-series.points.map(p => p.at(0)),
      panic-series.points.map(p => p.at(1)),
      mark: none, stroke: blue + 1.5pt, label: [Panic margin]),
    lq.plot(panic-series.kicks.map(p => p.at(0)),
      panic-series.kicks.map(p => p.at(1)),
      stroke: none, mark: "o", color: orange,
      label: [Local-input mismatch]),
  )
}
