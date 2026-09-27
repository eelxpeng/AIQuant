"""Plots for the statistics package.

Kept separate so the test modules never import matplotlib -- they are the part
that gets imported from notebooks and other code.

The palette is the validated categorical default: slot 1 blue, slot 2 orange,
which clears the colour-vision-deficiency and contrast gates on both the light
and dark surfaces. Reference lines and bands are drawn in neutral ink so they
never read as a third data series.
"""

import os
from pathlib import Path

import numpy as np
import pandas as pd

import matplotlib

if not os.environ.get("DISPLAY"):
    matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402

LIGHT = {
    "surface": "#fcfcfb", "primary": "#0b0b0b", "secondary": "#52514e",
    "muted": "#898781", "grid": "#e1e0d9", "axis": "#c3c2b7",
    "series": ("#2a78d6", "#eb6834", "#1baf7a"),
}
DARK = {
    "surface": "#1a1a19", "primary": "#ffffff", "secondary": "#c3c2b7",
    "muted": "#898781", "grid": "#2c2c2a", "axis": "#383835",
    "series": ("#3987e5", "#d95926", "#199e70"),
}

# Only the first three categorical slots clear the colour-vision gates for every
# pair. A fourth leg would put yellow beside orange, so extra legs are dropped
# from the legs panel -- and the panel says so rather than silently truncating.
MAX_LEGS_PLOTTED = 3


def _style(ax, palette, ylabel=""):
    """Recessive grid and axes; muted tick ink."""
    ax.set_facecolor(palette["surface"])
    ax.grid(True, color=palette["grid"], linewidth=0.8, alpha=0.9)
    ax.set_axisbelow(True)
    for side in ("top", "right"):
        ax.spines[side].set_visible(False)
    for side in ("left", "bottom"):
        ax.spines[side].set_color(palette["axis"])
        ax.spines[side].set_linewidth(1.0)
    ax.tick_params(colors=palette["muted"], labelsize=9, length=0)
    if ylabel:
        ax.set_ylabel(ylabel, color=palette["secondary"], fontsize=10)


def _legend(ax, palette, loc="upper left"):
    leg = ax.legend(frameon=False, fontsize=9, loc=loc, handlelength=1.8, borderpad=0)
    for text in leg.get_texts():
        text.set_color(palette["secondary"])
    return leg


def plot_panels(
    panels,
    path=None,
    dark: bool = False,
    title: str = "",
    subtitle: str = "",
    bands=(),
    band_window: int | None = None,
    hlines=None,
    show: bool = False,
):
    """Stack one panel per series, sharing the x-axis.

    ``panels`` is a mapping of label -> Series. Each gets its own panel rather
    than its own y-axis: two scales on one plot (``twinx``) make the crossings
    and relative levels mean something they do not, and it is the easiest chart
    mistake to make. Stacked panels keep the shared time axis and give each
    series an honest scale.

    ``bands`` names the panels that get mean +/- 1 and 2 sigma reference bands;
    ``band_window`` makes those bands roll over that many periods instead of
    spanning the whole sample. ``hlines`` maps a panel label to levels drawn as
    horizontal reference rules -- entry and exit thresholds, say.
    """
    palette = DARK if dark else LIGHT
    panels = dict(panels)
    # Reserve the header in inches, not figure fractions -- a fraction that
    # clears the title at three panels collides with it at one.
    header_in = 0.95 if title else 0.2
    height = 3.1 * len(panels) + header_in
    fig, axes = plt.subplots(
        len(panels), 1, figsize=(11, height), sharex=True, squeeze=False
    )
    axes = axes[:, 0]
    fig.patch.set_facecolor(palette["surface"])

    for ax, colour, (label, series) in zip(axes, palette["series"], panels.items()):
        series = series.dropna()
        if label in bands:
            if band_window:
                centre = series.rolling(band_window).mean()
                spread = series.rolling(band_window).std()
            else:
                centre = pd.Series(series.mean(), index=series.index)
                spread = pd.Series(series.std(), index=series.index)
            for k, alpha in ((2, 0.10), (1, 0.16)):
                ax.fill_between(series.index, centre - k * spread, centre + k * spread,
                                color=palette["muted"], alpha=alpha, linewidth=0)
            ax.plot(centre.index, centre.to_numpy(), color=palette["axis"],
                    linewidth=1.2, linestyle="--")
        for level in (hlines or {}).get(label, ()):
            ax.axhline(level, color=palette["axis"], linewidth=1.0,
                       linestyle="--" if level else "-")
        ax.plot(series.index, series.to_numpy(), color=colour, linewidth=1.6)
        # One series per panel, so the title names it -- no legend box, and no
        # y-axis label repeating the same words down the side.
        _style(ax, palette)
        ax.set_title(label, color=palette["secondary"], fontsize=10, loc="left", pad=6)

    if title:
        fig.suptitle(title, color=palette["primary"], fontsize=15, fontweight="bold",
                     x=0.065, ha="left", y=1 - 0.32 / height)
    if subtitle:
        fig.text(0.065, 1 - 0.62 / height, subtitle,
                 color=palette["secondary"], fontsize=10, ha="left")
    fig.tight_layout(rect=[0, 0, 1, 1 - header_in / height])

    if show:
        plt.show()
    if path is None:
        return fig
    path = Path(path)
    fig.savefig(path, dpi=140, facecolor=palette["surface"])
    plt.close(fig)
    return path


def plot_spread(
    result,
    path=None,
    dark: bool = False,
    acf_lags: int = 120,
    show: bool = False,
):
    """Draw a cointegrated spread and how fast it reverts.

    Takes anything exposing ``legs`` (a DataFrame of the aligned level series),
    ``spread``, ``spread_label``, ``plot_headline``, ``plot_subtitle`` and
    ``spread_half_life`` -- so both a two-series Engle-Granger result and an
    n-series Johansen result render through the same path.

    Three panels: the legs indexed to a common base (so one y-axis serves them
    all -- never two scales on one plot), the spread with its mean and sigma
    bands, and the autocorrelation decay against the curve implied by the
    fitted half-life.

    Returns the path written, or the Figure when ``path`` is None.
    """
    palette = DARK if dark else LIGHT
    spread = result.spread
    mu, sigma = float(spread.mean()), float(spread.std())
    hl = result.spread_half_life
    legs = result.legs

    fig, (ax_legs, ax_spread, ax_acf) = plt.subplots(
        3, 1, figsize=(11, 10.5), height_ratios=[1, 1.35, 1]
    )
    fig.patch.set_facecolor(palette["surface"])

    # --- Panel 1: the legs, indexed to 0 at the start ------------------------
    shown = legs.columns[:MAX_LEGS_PLOTTED]
    for colour, name in zip(palette["series"], shown):
        indexed = legs[name] - legs[name].iloc[0]
        ax_legs.plot(indexed.index, indexed.to_numpy(), color=colour, linewidth=2, label=str(name))
        # Direct labels are not decoration here: light-mode aqua sits below 3:1
        # against the surface, and a visible label is what the relief rule wants.
        ax_legs.annotate(
            f" {name}", (indexed.index[-1], indexed.iloc[-1]),
            color=palette["secondary"], fontsize=9, va="center",
            xytext=(4, 0), textcoords="offset points",
        )
    _style(ax_legs, palette, "cumulative log return")
    _legend(ax_legs, palette)
    dropped = len(legs.columns) - len(shown)
    title = "The legs, indexed to the start" + (
        f" — showing {len(shown)} of {len(legs.columns)}" if dropped else ""
    )
    ax_legs.set_title(title, color=palette["secondary"], fontsize=10, loc="left", pad=8)

    # --- Panel 2: the spread, with mean and sigma bands ----------------------
    for k, alpha in ((2, 0.10), (1, 0.16)):
        ax_spread.fill_between(
            spread.index, mu - k * sigma, mu + k * sigma,
            color=palette["muted"], alpha=alpha, linewidth=0,
        )
    ax_spread.axhline(mu, color=palette["axis"], linewidth=1.2, linestyle="--")
    ax_spread.plot(spread.index, spread.to_numpy(), color=palette["series"][0], linewidth=1.6)
    right = spread.index[-1]
    for level, text in ((mu, "mean"), (mu + sigma, "+1σ"), (mu - sigma, "−1σ"),
                        (mu + 2 * sigma, "+2σ"), (mu - 2 * sigma, "−2σ")):
        ax_spread.annotate(
            f" {text}", (right, level), color=palette["muted"], fontsize=8,
            va="center", xytext=(4, 0), textcoords="offset points",
        )
    _style(ax_spread, palette, result.spread_label)
    ax_spread.set_title(
        "The spread — excursions from the mean, and the return to it",
        color=palette["secondary"], fontsize=10, loc="left", pad=8,
    )

    # --- Panel 3: how fast it reverts ---------------------------------------
    from statsmodels.tsa.stattools import acf

    nlags = int(min(acf_lags, max(10, len(spread) // 4)))
    empirical = acf(spread.to_numpy(), nlags=nlags, fft=True)
    lags = np.arange(nlags + 1)
    bound = 1.96 / np.sqrt(len(spread))
    ax_acf.fill_between(lags, -bound, bound, color=palette["muted"], alpha=0.16, linewidth=0)
    ax_acf.axhline(0, color=palette["axis"], linewidth=1.0)
    ax_acf.plot(lags, empirical, color=palette["series"][0], linewidth=2,
                label="observed autocorrelation")
    if hl is not None and hl.reverts:
        ax_acf.plot(
            lags, 0.5 ** (lags / hl.half_life), color=palette["series"][1],
            linewidth=2, linestyle="--",
            label=f"implied by a {hl.half_life:.0f}-day half-life",
        )
        ax_acf.axvline(hl.half_life, color=palette["axis"], linewidth=1.0, linestyle=":")
        ax_acf.annotate(
            f" half-life {hl.half_life:.0f}d", (hl.half_life, 0.85),
            color=palette["secondary"], fontsize=9, xytext=(4, 0), textcoords="offset points",
        )
    _style(ax_acf, palette, "autocorrelation")
    ax_acf.set_xlabel("lag (trading days)", color=palette["secondary"], fontsize=10)
    ax_acf.set_xlim(0, nlags)
    _legend(ax_acf, palette, loc="upper right")
    ax_acf.set_title(
        "How fast it comes back — shaded band is where autocorrelation is "
        "indistinguishable from zero",
        color=palette["secondary"], fontsize=10, loc="left", pad=8,
    )

    fig.suptitle(
        result.plot_headline,
        color=palette["primary"], fontsize=15, fontweight="bold", x=0.065, ha="left", y=0.985,
    )
    fig.text(
        0.065, 0.958, result.plot_subtitle,
        color=palette["secondary"], fontsize=10, ha="left",
    )
    fig.tight_layout(rect=[0, 0, 1, 0.945])

    if show:
        plt.show()
    if path is None:
        return fig
    path = Path(path)
    fig.savefig(path, dpi=140, facecolor=palette["surface"])
    plt.close(fig)
    return path
