//! Status bar: per ogni account il nome, due mini barre sovrapposte (5h sopra, Week
//! sotto) del rimanente e la percentuale della finestra piu' stretta, coi colori di
//! `superzed_sessions::ui::pct_color` (giallo sotto il 20%, rosso sotto il 10%); tooltip
//! con le barre e i reset di tutti gli account (`limits_tooltip`); icona con tooltip se
//! le quote non si leggono; click → pannello Sessions con la sezione «Limits» aperta.
use crate::meter::{self, meter};
use crate::quota::{self, Quota};
use crate::{Account, AccountStatus, AccountStore};
use chrono::{DateTime, Utc};
use gpui::{Context, IntoElement, Render, SharedString, Subscription, Window};
use ui::{Icon, IconName, IconSize, Tooltip, prelude::*};
use workspace::{HideStatusItem, StatusItemView, item::ItemHandle};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuotaLevel {
    Normal,
    /// Sotto il 20% rimanente.
    Low,
    /// Sotto il 10% rimanente.
    Critical,
}

impl QuotaLevel {
    fn from_pct(pct: Option<u8>) -> Self {
        match pct.map(superzed_sessions::ui::pct_color) {
            Some(Color::Error) => Self::Critical,
            Some(Color::Warning) => Self::Low,
            _ => Self::Normal,
        }
    }

    /// Colore dei numeri: pieno quando non e' in allerta.
    pub fn color(self) -> Color {
        match self {
            Self::Normal => Color::Default,
            Self::Low => Color::Warning,
            Self::Critical => Color::Error,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StatusEntry {
    pub id: String,
    /// `label` seguito da `numbers`.
    pub text: String,
    pub label: String,
    pub numbers: String,
    /// Rimanente delle due finestre e della piu' stretta (per le barre e «NN%»).
    pub five: Option<u8>,
    pub week: Option<u8>,
    /// Limiti per modello (`weekly_scoped`, es. "Fable"): etichetta e rimanente, nell'ordine
    /// ricevuto. Al massimo il primo diventa una barra in status bar (task 19, punto 3); tutti
    /// contano per `tightest` e finiscono nel tooltip (`limits_tooltip`).
    pub scoped: Vec<(String, Option<u8>)>,
    pub tightest: Option<u8>,
    pub level: QuotaLevel,
    pub problem: Option<String>,
}

pub fn status_entry(account: &Account, quota: Option<&Quota>, status: &AccountStatus, now: DateTime<Utc>) -> StatusEntry {
    let five = quota::remaining_pct(quota.and_then(|q| q.session.as_ref()), now);
    let week = quota::remaining_pct(quota.and_then(|q| q.weekly.as_ref()), now);
    let scoped_limits: &[(String, quota::Limit)] = quota.map_or(&[], |q| &q.scoped);
    let scoped: Vec<(String, Option<u8>)> =
        scoped_limits.iter().map(|(name, limit)| (name.clone(), quota::remaining_pct(Some(limit), now))).collect();
    let pct = |p: Option<u8>| p.map(|p| format!("{p}%")).unwrap_or_else(|| "–".into());
    let numbers = format!("5h {} · week {}", pct(five), pct(week));
    let mut all = vec![five, week];
    all.extend(scoped.iter().map(|(_, r)| *r));
    let tightest = meter::tightest(&all);
    StatusEntry {
        id: account.id.clone(),
        text: format!("{} {numbers}", account.label),
        label: account.label.clone(),
        numbers,
        five,
        week,
        scoped,
        tightest,
        level: QuotaLevel::from_pct(tightest),
        problem: status.problem(),
    }
}

pub struct QuotaStatusItem {
    _subscription: Option<Subscription>,
}

impl QuotaStatusItem {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let subscription = AccountStore::global(cx).map(|s| cx.observe(&s, |_, _, cx| cx.notify()));
        Self { _subscription: subscription }
    }
}

impl Render for QuotaStatusItem {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(store) = AccountStore::global(cx) else {
            return div().into_any_element();
        };
        let store = store.read(cx);
        let now = store.now();
        let entries: Vec<StatusEntry> = store
            .accounts()
            .iter()
            .map(|a| status_entry(a, store.quota(&a.id), &store.status(&a.id), now))
            .collect();
        let hover = cx.theme().colors().ghost_element_hover;
        // mini barre: 2.5rem x 3px; 2px fra l'una e l'altra con due barre (5h/Week), 1px
        // con tre (piu' un limite per modello, es. Fable), per restare dentro la status bar.
        let (bar_w, bar_h) = (rems(2.5), rems(0.1875));
        let bar_gap = |extra_bars: usize| if extra_bars > 0 { rems(0.0625) } else { rems(0.125) };
        h_flex()
            .id("superzed-quota")
            .debug_selector(|| "superzed-quota".into())
            .gap_1p5()
            .cursor_pointer()
            .on_click(|_, window, cx| window.dispatch_action(Box::new(superzed_actions::ShowAccountLimits), cx))
            .children(entries.into_iter().map(|e| {
                let selector = format!("superzed-quota-{}", e.id);
                let problem_id = SharedString::from(format!("superzed-quota-problem-{}", e.id));
                let (five_sel, week_sel, pct_sel) = (
                    format!("superzed-quota-bar-{}-5h", e.id),
                    format!("superzed-quota-bar-{}-week", e.id),
                    format!("superzed-quota-pct-{}", e.id),
                );
                let (pct, pct_color) = meter::pct_label(e.tightest);
                // al massimo una barra in piu' per un limite per modello (es. Fable): le
                // altre, se ce ne fossero, restano solo nel tooltip.
                let extra_bar = e.scoped.first().map(|(name, remaining)| {
                    let selector = format!("superzed-quota-bar-{}-{}", e.id, meter::slug(name));
                    meter(*remaining, bar_w, bar_h, cx).debug_selector(move || selector)
                });
                h_flex()
                    .id(SharedString::from(selector.clone()))
                    .gap_1()
                    .px_1()
                    .rounded_sm()
                    .hover(move |d| d.bg(hover))
                    .debug_selector(move || selector)
                    .tooltip(crate::limits_tooltip::limits_tooltip)
                    .child(Label::new(e.label).size(LabelSize::Small).color(Color::Muted))
                    .child(
                        v_flex()
                            .flex_none()
                            .gap(bar_gap(extra_bar.is_some() as usize))
                            .child(meter(e.five, bar_w, bar_h, cx).debug_selector(move || five_sel))
                            .child(meter(e.week, bar_w, bar_h, cx).debug_selector(move || week_sel))
                            .children(extra_bar),
                    )
                    .child(div().debug_selector(move || pct_sel).child(Label::new(pct).size(LabelSize::Small).color(pct_color)))
                    .children(e.problem.map(|p| {
                        let selector = problem_id.to_string();
                        div()
                            .id(problem_id.clone())
                            .debug_selector(move || selector)
                            .child(Icon::new(IconName::Warning).size(IconSize::XSmall).color(Color::Warning))
                            .tooltip(Tooltip::text(p))
                    }))
            }))
            .into_any_element()
    }
}

impl StatusItemView for QuotaStatusItem {
    fn set_active_pane_item(&mut self, _: Option<&dyn ItemHandle>, _: &mut Window, _: &mut Context<Self>) {}

    // sempre presente (almeno il sistema); nessuna voce "nascondi" nelle impostazioni
    fn hide_setting(&self, _: &gpui::App) -> Option<HideStatusItem> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::Limit;
    use gpui::{Modifiers, TestAppContext};

    fn q(session: f32, weekly: Option<f32>) -> Quota {
        let now = Utc::now();
        let l = |u: f32| Limit { used_percent: u, resets_at: None, seen_at: now };
        Quota { session: Some(l(session)), weekly: weekly.map(l), ..Default::default() }
    }

    #[test]
    fn entries_show_remaining_and_color_by_the_tighter_window() {
        let a = Account { id: "a1".into(), label: "j.doe".into(), email: None, config_dir: None, exclude_from: Vec::new() };
        let now = Utc::now();
        let e = status_entry(&a, Some(&q(37., Some(20.))), &AccountStatus::Ok, now);
        assert_eq!(e.text, "j.doe 5h 63% · week 80%");
        assert_eq!((e.level, e.problem), (QuotaLevel::Normal, None));
        assert_eq!(status_entry(&a, Some(&q(85., Some(10.))), &AccountStatus::Ok, now).level, QuotaLevel::Low);
        assert_eq!(status_entry(&a, Some(&q(10., Some(96.))), &AccountStatus::Ok, now).level, QuotaLevel::Critical);
        // soglie comuni (pct_color): sotto il 10% e' gia' critico, 10% ancora basso
        assert_eq!(status_entry(&a, Some(&q(92., None)), &AccountStatus::Ok, now).level, QuotaLevel::Critical);
        assert_eq!(status_entry(&a, Some(&q(90., None)), &AccountStatus::Ok, now).level, QuotaLevel::Low);
        let unknown = status_entry(&a, None, &AccountStatus::NoToken, now);
        assert_eq!(unknown.text, "j.doe 5h – · week –");
        assert_eq!(unknown.level, QuotaLevel::Normal);
        assert!(unknown.problem.unwrap().contains("Keychain"));
        let expired = status_entry(&a, Some(&q(50., None)), &AccountStatus::TokenExpired, now);
        assert_eq!(expired.text, "j.doe 5h 50% · week –", "le quote in cache restano");
        assert!(expired.problem.is_some());
    }

    // Task 19: un limite per modello (`weekly_scoped`, es. Fable) piu' stretto di 5h/Week
    // diventa il `tightest` mostrato in status bar.
    #[test]
    fn a_model_limit_can_be_the_tighter_window() {
        let a = Account { id: "a1".into(), label: "j.doe".into(), email: None, config_dir: None, exclude_from: Vec::new() };
        let now = Utc::now();
        let l = |u: f32| Limit { used_percent: u, resets_at: None, seen_at: now };
        let quota = Quota { session: Some(l(37.)), weekly: Some(l(20.)), scoped: vec![("Fable".to_string(), l(91.))], ..Default::default() };
        let e = status_entry(&a, Some(&quota), &AccountStatus::Ok, now);
        assert_eq!(e.scoped, vec![("Fable".to_string(), Some(9))]);
        assert_eq!(e.tightest, Some(9), "Fable al 9% rimanente batte 5h (63%) e week (80%)");
        assert_eq!(e.level, QuotaLevel::Critical, "il livello segue anch'esso il limite per modello piu' stretto");
    }

    // Task 23: il tooltip non e' piu' testo semplice: per ogni account (prima il piu'
    // stretto) nome, poi per ogni finestra nome, «NN% left», barra a tutta larghezza e
    // «Resets in ...»; «No data» per chi non ha quote.
    #[gpui::test]
    fn hovering_the_item_shows_a_tooltip_with_a_bar_per_window(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings = settings::SettingsStore::test(cx);
            cx.set_global(settings);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            let (_store, _tokens, fetcher) = crate::test_support::fixture_store(cx, dir.path(), &[(37., 20.)]);
            fetcher.set("tok-a1", crate::FetchOutcome::Ok(crate::test_support::usage_json_scoped(37., 20., &[("Fable", 91.)])));
        });
        cx.run_until_parked();
        let (_item, cx) = cx.add_window_view(|_, cx| QuotaStatusItem::new(cx));
        cx.run_until_parked();
        let sel = |what: String| -> &'static str { Box::leak(format!("superzed-quota-tooltip-{what}").into_boxed_str()) };
        assert!(cx.debug_bounds(sel("a1".into())).is_none(), "nessun tooltip prima del passaggio del mouse");
        let item = cx.debug_bounds("superzed-quota-a1").unwrap();
        cx.simulate_mouse_move(item.center(), None, Modifiers::default());
        cx.executor().advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        let tooltip = cx.debug_bounds("superzed-quota-tooltip").expect("tooltip ricco");
        let mini = cx.debug_bounds("superzed-quota-bar-a1-5h").unwrap();
        let mut bars = Vec::new();
        for key in ["5h", "week", "fable"] {
            let bar = cx.debug_bounds(sel(format!("bar-a1-{key}"))).unwrap_or_else(|| panic!("barra {key} nel tooltip"));
            assert!(cx.debug_bounds(sel(format!("pct-a1-{key}"))).is_some(), "percentuale {key}");
            let reset = cx.debug_bounds(sel(format!("reset-a1-{key}"))).unwrap_or_else(|| panic!("reset {key}"));
            assert!(reset.top() >= bar.bottom(), "reset sotto la barra {key}");
            assert!(bar.size.width > mini.size.width * 2., "barra a tutta larghezza, non la mini barra");
            bars.push(bar);
        }
        assert!(bars[0].bottom() <= bars[1].top() && bars[1].bottom() <= bars[2].top(), "5h, Week, Fable in colonna");
        assert!(cx.debug_bounds(sel("login-needed-system".into())).is_some(), "sistema senza token: «Login needed»");
        let (a1, system) = (cx.debug_bounds(sel("a1".into())).unwrap(), cx.debug_bounds(sel("system".into())).unwrap());
        assert!(a1.top() < system.top(), "prima l'account piu' stretto, senza dati in fondo");
        assert!(tooltip.contains(&bars[0].origin), "le barre stanno nel tooltip");
    }

    #[test]
    fn the_tooltip_reset_line_is_the_countdown() {
        use chrono::{Local, TimeZone as _};
        let a = Account { id: "a1".into(), label: "j.doe".into(), email: Some("g@x.it".into()), config_dir: None, exclude_from: Vec::new() };
        let local = |d, h, m| Local.with_ymd_and_hms(2026, 9, d, h, m, 0).unwrap().with_timezone(&Utc);
        let now = local(27, 10, 0);
        let quota = Quota {
            session: Some(Limit { used_percent: 17., resets_at: Some(local(27, 12, 4)), seen_at: now }),
            weekly: Some(Limit { used_percent: 88., resets_at: Some(local(29, 4, 34)), seen_at: now }),
            ..Default::default()
        };
        let row = meter::limits_row(&a, Some(&quota), &AccountStatus::Ok, now);
        let lines: Vec<_> = row.windows().into_iter().map(|(_, name, w)| (name, meter::left_label(w.remaining), w.reset.clone())).collect();
        assert_eq!(
            lines,
            [
                ("5h".to_string(), "83% left".to_string(), Some("Resets in 2h 4m".to_string())),
                ("Week".to_string(), "12% left".to_string(), Some("Resets in 1d 18h 34m".to_string())),
            ]
        );
        assert_eq!(meter::left_label(None), "–");
    }

    #[gpui::test]
    fn the_item_shows_two_bars_per_account_and_a_click_opens_the_limits(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings = settings::SettingsStore::test(cx);
            cx.set_global(settings);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            crate::test_support::fixture_store(cx, dir.path(), &[(37., 20.)]);
        });
        cx.run_until_parked();
        let (_item, cx) = cx.add_window_view(|_, cx| QuotaStatusItem::new(cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("superzed-quota-system").is_some());
        assert!(cx.debug_bounds("superzed-quota-a1").is_some());
        assert!(cx.debug_bounds("superzed-quota-problem-system").is_some(), "sistema senza token: icona");
        // due barre per account, 5h sopra e Week sotto, e la percentuale della piu' stretta
        for id in ["a1", "system"] {
            let five = cx.debug_bounds(Box::leak(format!("superzed-quota-bar-{id}-5h").into_boxed_str())).expect("barra 5h");
            let week = cx.debug_bounds(Box::leak(format!("superzed-quota-bar-{id}-week").into_boxed_str())).expect("barra week");
            assert!(five.bottom() <= week.top(), "5h sopra Week");
            assert_eq!(five.size.width, week.size.width);
        }
        assert!(cx.debug_bounds("superzed-quota-pct-a1").is_some());
        let opened = std::rc::Rc::new(std::cell::Cell::new(false));
        let o = opened.clone();
        // `cx.on_action` restituisce `&mut Self` per l'incatenamento: nella closure di `update`
        // quel riferimento non può sopravvivere alla chiamata, quindi si scarta con `;`.
        cx.update(|_, cx| {
            cx.on_action(move |_: &superzed_actions::ShowAccountLimits, _| o.set(true));
        });
        let item = cx.debug_bounds("superzed-quota").unwrap();
        cx.simulate_click(item.center(), Modifiers::default());
        cx.run_until_parked();
        assert!(opened.get(), "click → Sessions con Limits");
    }

    // Task 19: un account con un limite per modello (Fable) mostra tre barre impilate in
    // status bar (5h, Week, Fable), oltre alle due degli altri account.
    #[gpui::test]
    fn an_account_with_a_model_limit_shows_a_third_bar(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings = settings::SettingsStore::test(cx);
            cx.set_global(settings);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            let (_store, _tokens, fetcher) = crate::test_support::fixture_store(cx, dir.path(), &[(37., 20.)]);
            fetcher.set("tok-a1", crate::FetchOutcome::Ok(crate::test_support::usage_json_scoped(37., 20., &[("Fable", 91.)])));
        });
        cx.run_until_parked();
        let (_item, cx) = cx.add_window_view(|_, cx| QuotaStatusItem::new(cx));
        cx.run_until_parked();
        let five = cx.debug_bounds("superzed-quota-bar-a1-5h").expect("barra 5h");
        let week = cx.debug_bounds("superzed-quota-bar-a1-week").expect("barra week");
        let fable = cx.debug_bounds("superzed-quota-bar-a1-fable").expect("barra Fable");
        assert!(five.bottom() <= week.top(), "5h sopra Week");
        assert!(week.bottom() <= fable.top(), "Fable sotto Week");
        assert_eq!(five.size.width, fable.size.width, "stessa larghezza delle altre barre");
        // il tooltip nomina il modello
        assert!(cx.debug_bounds("superzed-quota-a1").is_some());
        // "system" resta a due barre (nessun limite per modello)
        assert!(cx.debug_bounds("superzed-quota-bar-system-5h").is_some());
        assert!(cx.debug_bounds("superzed-quota-bar-system-fable").is_none());
    }
}
