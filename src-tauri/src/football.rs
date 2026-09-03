//! Physics-driven football match for the `football` social scene.
//!
//! Unlike the scripted prop plays, the ball here is a real entity with
//! velocity, rolling friction and wall bounces, and every pet owns a collision
//! volume (its window rectangle). Pets steer with a light team AI — the
//! nearest one charges the ball, the second cuts off the ball's future path
//! and the rest flank — and touches are classified by context: gentle
//! contacts dribble the ball ahead, a charged carrier shoots, and a contact
//! by a pet without possession tackles the ball clear. A two, three, or four
//! pet match therefore emerges from the simulation instead of a fixed
//! choreography.
//!
//! All coordinates are logical pixels. The ball position is the centre of the
//! 72x72 prop window; pet positions are window top-left corners, matching the
//! rest of the social runtime.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use rand::Rng;
use serde_json::json;
use tauri::{AppHandle, Emitter, LogicalPosition, Manager};

use super::PetPosition;

const TICK_MS: u64 = 40;
const TICK_S: f64 = TICK_MS as f64 / 1000.0;

const BALL_RADIUS: f64 = 18.0;
/// Rolling deceleration in px/s^2. Strong enough that a stray ball settles
/// within a couple of seconds, keeping the match on the stage.
const BALL_FRICTION: f64 = 260.0;
/// Below this speed the ball counts as stopped and steering snaps to it.
const BALL_REST_SPEED: f64 = 30.0;
const WALL_RESTITUTION: f64 = 0.68;
const KICK_SPEED_MIN: f64 = 460.0;
const KICK_SPEED_MAX: f64 = 720.0;
const KICK_COOLDOWN_S: f64 = 0.45;
/// The ball is pushed this far out of the kicker on contact so the next tick
/// cannot re-trigger the same overlap before the cooldown even matters.
const KICK_ESCAPE: f64 = 8.0;
const CHASER_SPEED: f64 = 150.0;
const SUPPORT_SPEED: f64 = 118.0;
const SUPPORT_SPREAD: f64 = 95.0;
/// Second-nearest pet chases the ball's future position to attempt tackles.
const INTERCEPT_SPEED: f64 = 168.0;
const INTERCEPT_LOOKAHEAD_S: f64 = 0.35;
const PLAYER_SEPARATION_GAP: f64 = 6.0;
const ARRIVAL_EPSILON: f64 = 6.0;
const MATCH_PHASE_INTERVAL_MS: u64 = 700;
const MATCH_SAY_COOLDOWN_MS: u64 = 2_600;
const SAY_CHANCE: f64 = 0.65;

// Dribbling: slow contacts nudge the ball ahead instead of blasting it away,
// so a pet can carry the ball until it charges a shot or gets tackled.
/// Consecutive gentle touches before the carrier blasts a shot.
const SHOOT_CHARGE_AFTER_TOUCHES: u32 = 3;
/// Small chance any gentle touch turns into an early shot.
const SHOOT_CHANCE: f64 = 0.15;
const DRIBBLE_PUSH_FACTOR: f64 = 1.35;
const DRIBBLE_MIN_PUSH: f64 = 90.0;
const DRIBBLE_PUSH_BONUS: f64 = 110.0;
const DRIBBLE_COOLDOWN_S: f64 = 0.12;
const DRIBBLE_ESCAPE: f64 = 6.0;
/// Below this travel speed a pet is considered stationary for push direction.
const TRAVEL_SPEED_EPSILON: f64 = 40.0;

pub(crate) struct FootballPlayer {
    pub instance_id: String,
    pub pet_id: String,
    pub say: String,
    pub width: f64,
    pub height: f64,
}

/// Axis-aligned bounds in logical pixels. `min_x/max_x` bound ball centres
/// for the pitch, and window top-left corners for the player area.
#[derive(Clone, Copy)]
pub(crate) struct PitchBounds {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl PitchBounds {
    fn clamp_point(&self, x: f64, y: f64) -> (f64, f64) {
        (
            x.clamp(self.min_x, self.max_x.max(self.min_x)),
            y.clamp(self.min_y, self.max_y.max(self.min_y)),
        )
    }
}

struct Ball {
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
}

impl Ball {
    fn speed(&self) -> f64 {
        self.vx.hypot(self.vy)
    }

    /// Integrate one tick; returns true when a wall bounce happened.
    fn step(&mut self, pitch: &PitchBounds) -> bool {
        self.x += self.vx * TICK_S;
        self.y += self.vy * TICK_S;
        let mut bounced = false;
        if self.x <= pitch.min_x {
            self.x = pitch.min_x;
            if self.vx < 0.0 {
                self.vx = -self.vx * WALL_RESTITUTION;
                bounced = true;
            }
        } else if self.x >= pitch.max_x {
            self.x = pitch.max_x;
            if self.vx > 0.0 {
                self.vx = -self.vx * WALL_RESTITUTION;
                bounced = true;
            }
        }
        if self.y <= pitch.min_y {
            self.y = pitch.min_y;
            if self.vy < 0.0 {
                self.vy = -self.vy * WALL_RESTITUTION;
                bounced = true;
            }
        } else if self.y >= pitch.max_y {
            self.y = pitch.max_y;
            if self.vy > 0.0 {
                self.vy = -self.vy * WALL_RESTITUTION;
                bounced = true;
            }
        }
        let speed = self.speed();
        if speed > 0.0 {
            let next = (speed - BALL_FRICTION * TICK_S).max(0.0);
            if next < BALL_REST_SPEED {
                self.vx = 0.0;
                self.vy = 0.0;
            } else {
                let scale = next / speed;
                self.vx *= scale;
                self.vy *= scale;
            }
        }
        bounced
    }
}

/// Launch the ball away from the kicking player. Falls back to the ball's
/// current heading when the ball sits exactly on the player's centre, and to
/// a straight shot when it is dead still, so a kick never produces zero
/// velocity. A small angular jitter keeps volleys from looking scripted.
fn kick_velocity(
    player_center: (f64, f64),
    ball_center: (f64, f64),
    current_velocity: (f64, f64),
) -> (f64, f64) {
    let mut dx = ball_center.0 - player_center.0;
    let mut dy = ball_center.1 - player_center.1;
    let distance = dx.hypot(dy);
    if distance < 1.0 {
        let speed = current_velocity.0.hypot(current_velocity.1);
        if speed > 1.0 {
            dx = current_velocity.0 / speed;
            dy = current_velocity.1 / speed;
        } else {
            dx = 1.0;
            dy = 0.0;
        }
    } else {
        dx /= distance;
        dy /= distance;
    }
    let jitter: f64 = (rand::rng().random_range(0.0..1.0) - 0.5) * 0.42;
    let (sin, cos) = jitter.sin_cos();
    let power = rand::rng().random_range(KICK_SPEED_MIN..KICK_SPEED_MAX);
    let dir_x = dx * cos - dy * sin;
    let dir_y = dx * sin + dy * cos;
    (dir_x * power, dir_y * power)
}

/// What happens when a pet's collision volume touches the ball.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TouchKind {
    /// Gentle nudge ahead of the pet: the ball rolls a little, the pet
    /// catches up, and the run continues.
    Dribble,
    /// Full-power strike down the pet's travel direction.
    Shoot,
    /// A pet without possession knocked the loose ball away at full power.
    Tackle,
}

fn classify_touch(
    toucher: usize,
    possession: Option<usize>,
    dribble_touches: u32,
    roll: f64,
) -> TouchKind {
    if possession.is_some_and(|carrier| carrier != toucher) {
        return TouchKind::Tackle;
    }
    if dribble_touches + 1 >= SHOOT_CHARGE_AFTER_TOUCHES || roll < SHOOT_CHANCE {
        TouchKind::Shoot
    } else {
        TouchKind::Dribble
    }
}

/// The direction a touch pushes the ball: the pet's actual movement while it
/// runs, or straight at the ball when it is standing still.
fn travel_direction(
    velocity: (f64, f64),
    center: (f64, f64),
    ball_center: (f64, f64),
) -> (f64, f64) {
    let speed = velocity.0.hypot(velocity.1);
    if speed > TRAVEL_SPEED_EPSILON {
        return (velocity.0 / speed, velocity.1 / speed);
    }
    let dx = ball_center.0 - center.0;
    let dy = ball_center.1 - center.1;
    let distance = dx.hypot(dy);
    if distance < 1.0 {
        (1.0, 0.0)
    } else {
        (dx / distance, dy / distance)
    }
}

/// A dribble touch: slightly faster than the pet so the ball rolls ahead and
/// the pet catches up, which reads as controlled footwork instead of a kick.
fn dribble_push(
    velocity: (f64, f64),
    center: (f64, f64),
    ball_center: (f64, f64),
) -> (f64, f64) {
    let (direction, power) = {
        let speed = velocity.0.hypot(velocity.1);
        if speed > TRAVEL_SPEED_EPSILON {
            let power = (speed * DRIBBLE_PUSH_FACTOR).clamp(
                DRIBBLE_MIN_PUSH,
                speed + DRIBBLE_PUSH_BONUS,
            );
            (velocity, power)
        } else {
            (
                travel_direction(velocity, center, ball_center),
                DRIBBLE_MIN_PUSH,
            )
        }
    };
    let length = direction.0.hypot(direction.1).max(0.0001);
    (direction.0 / length * power, direction.1 / length * power)
}

/// A charged shot: the travel direction with a tighter jitter than a random
/// clearance, so shots visibly go where the pet was heading.
fn shoot_velocity(travel: (f64, f64)) -> (f64, f64) {
    let length = travel.0.hypot(travel.1).max(0.0001);
    let (dx, dy) = (travel.0 / length, travel.1 / length);
    let jitter: f64 = (rand::rng().random_range(0.0..1.0) - 0.5) * 0.24;
    let (sin, cos) = jitter.sin_cos();
    let power = rand::rng().random_range(KICK_SPEED_MIN..KICK_SPEED_MAX);
    let dir_x = dx * cos - dy * sin;
    let dir_y = dx * sin + dy * cos;
    (dir_x * power, dir_y * power)
}

/// Circle-vs-rectangle overlap: distance from the ball centre to the closest
/// point on the pet rectangle must stay within the ball radius.
fn touches_ball(
    player_top_left: (f64, f64),
    width: f64,
    height: f64,
    ball_center: (f64, f64),
) -> bool {
    let closest_x = ball_center
        .0
        .clamp(player_top_left.0, player_top_left.0 + width);
    let closest_y = ball_center
        .1
        .clamp(player_top_left.1, player_top_left.1 + height);
    let dx = ball_center.0 - closest_x;
    let dy = ball_center.1 - closest_y;
    dx.hypot(dy) <= BALL_RADIUS
}

/// Push overlapping pet rectangles apart along their minimum translation
/// axis, mirroring the social runtime's separation so windows never stack.
fn separate_players(positions: &mut [PetPosition], players: &[FootballPlayer]) {
    for _ in 0..6 {
        let mut changed = false;
        for first in 0..positions.len() {
            for second in (first + 1)..positions.len() {
                let (first_w, first_h) = (players[first].width, players[first].height);
                let (second_w, second_h) = (players[second].width, players[second].height);
                let overlap_x = (positions[first].x + first_w + PLAYER_SEPARATION_GAP)
                    .min(positions[second].x + second_w + PLAYER_SEPARATION_GAP)
                    - positions[first].x.max(positions[second].x);
                let overlap_y = (positions[first].y + first_h + PLAYER_SEPARATION_GAP)
                    .min(positions[second].y + second_h + PLAYER_SEPARATION_GAP)
                    - positions[first].y.max(positions[second].y);
                if overlap_x <= 0.0 || overlap_y <= 0.0 {
                    continue;
                }
                changed = true;
                let first_center_x = positions[first].x + first_w / 2.0;
                let second_center_x = positions[second].x + second_w / 2.0;
                let first_center_y = positions[first].y + first_h / 2.0;
                let second_center_y = positions[second].y + second_h / 2.0;
                if overlap_x <= overlap_y {
                    let direction = if second_center_x >= first_center_x {
                        1.0
                    } else {
                        -1.0
                    };
                    let push = overlap_x / 2.0;
                    positions[first].x -= direction * push;
                    positions[second].x += direction * push;
                } else {
                    let direction = if second_center_y >= first_center_y {
                        1.0
                    } else {
                        -1.0
                    };
                    let push = overlap_y / 2.0;
                    positions[first].y -= direction * push;
                    positions[second].y += direction * push;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

fn clamp_players(positions: &mut [PetPosition], players: &[FootballPlayer], area: &PitchBounds) {
    for (position, player) in positions.iter_mut().zip(players.iter()) {
        let (x, y) = area.clamp_point(position.x, position.y);
        position.x = x.min((area.max_x - player.width).max(area.min_x));
        position.y = y.min((area.max_y - player.height).max(area.min_y));
    }
}

/// One steering tick with three tiers: the nearest pet charges the ball
/// head-on, the second cuts off the ball's future path (which is how tackles
/// actually happen), and the rest aim beside and slightly behind the ball so
/// a group surrounds the play instead of forming a conga line. The per-pet
/// actual movement is written back so touch classification knows how fast
/// each pet was running.
fn steer_players(
    players: &[FootballPlayer],
    positions: &mut [PetPosition],
    ball: &Ball,
    velocities: &mut [(f64, f64)],
) {
    if players.is_empty() {
        return;
    }
    let distance_of = |index: usize| -> f64 {
        let player = &players[index];
        let center = (
            positions[index].x + player.width / 2.0,
            positions[index].y + player.height / 2.0,
        );
        (ball.x - center.0).hypot(ball.y - center.1)
    };
    let mut order: Vec<usize> = (0..players.len()).collect();
    order.sort_by(|&first, &second| distance_of(first).total_cmp(&distance_of(second)));

    let speed = ball.speed();
    let heading = if speed > 1.0 {
        (ball.vx / speed, ball.vy / speed)
    } else {
        (0.0, 0.0)
    };
    for (rank, &index) in order.iter().enumerate() {
        let player = &players[index];
        let center = (
            positions[index].x + player.width / 2.0,
            positions[index].y + player.height / 2.0,
        );
        let (target, pace) = match rank {
            0 => ((ball.x, ball.y), CHASER_SPEED),
            1 => (
                (
                    ball.x + ball.vx * INTERCEPT_LOOKAHEAD_S,
                    ball.y + ball.vy * INTERCEPT_LOOKAHEAD_S,
                ),
                INTERCEPT_SPEED,
            ),
            _ => {
                let perp = (-heading.1, heading.0);
                let side = if index % 2 == 0 { 1.0 } else { -1.0 };
                let spread = SUPPORT_SPREAD * (1.0 + index as f64 * 0.25);
                (
                    (
                        ball.x - heading.0 * 60.0 + perp.0 * spread * side,
                        ball.y - heading.1 * 60.0 + perp.1 * spread * side,
                    ),
                    SUPPORT_SPEED,
                )
            }
        };
        let dx = target.0 - center.0;
        let dy = target.1 - center.1;
        let distance = dx.hypot(dy);
        let step = if distance <= ARRIVAL_EPSILON {
            velocities[index] = (0.0, 0.0);
            continue;
        } else {
            (pace * TICK_S).min(distance)
        };
        positions[index].x += dx / distance * step;
        positions[index].y += dy / distance * step;
        velocities[index] = (dx / distance * step / TICK_S, dy / distance * step / TICK_S);
    }
}

fn read_window_position(app: &AppHandle, instance_id: &str) -> Option<PetPosition> {
    let label = super::instance_label(instance_id).ok()?;
    let window = app.get_webview_window(&label)?;
    let (Ok(position), Ok(scale)) = (window.outer_position(), window.scale_factor()) else {
        return None;
    };
    let logical: LogicalPosition<f64> = position.to_logical(scale);
    Some(PetPosition {
        x: logical.x,
        y: logical.y,
    })
}

/// Emit a `pet://social-phase` update so pet webviews run, face the ball and,
/// for the kicker, flash an effect or say a line. Shapes match the regular
/// social scene phase events consumed by `main.ts`.
#[allow(clippy::too_many_arguments)]
fn emit_match_phase(
    app: &AppHandle,
    scene_id: &str,
    players: &[FootballPlayer],
    positions: &[PetPosition],
    ball: &Ball,
    last_kicker: Option<usize>,
    speaker: Option<usize>,
) {
    let participants = players
        .iter()
        .enumerate()
        .map(|(index, player)| {
            let center = (
                positions[index].x + player.width / 2.0,
                positions[index].y + player.height / 2.0,
            );
            let look = super::social::look_direction_toward(center, (ball.x, ball.y));
            let is_speaker = speaker == Some(index);
            json!({
                "instanceId": player.instance_id,
                "petId": player.pet_id,
                "animation": "running",
                "look": look,
                "say": is_speaker.then(|| player.say.clone()),
                "effect": (last_kicker == Some(index)).then(|| "star".to_string()),
            })
        })
        .collect::<Vec<_>>();
    let _ = app.emit(
        "pet://social-phase",
        json!({
            "sceneId": scene_id,
            "phase": "interaction",
            "participants": participants,
        }),
    );
}

/// Run one match until `duration_ms` elapses or the scene is cancelled.
/// Returns the final pet top-left positions so the coordinator can persist
/// them, or `None` when cancelled.
pub(crate) async fn run_match(
    app: &AppHandle,
    scene_id: &str,
    players: &[FootballPlayer],
    prop_label: &str,
    ball_center: (f64, f64),
    pitch: PitchBounds,
    player_area: PitchBounds,
    duration_ms: u64,
    cancel: &AtomicBool,
) -> Option<Vec<PetPosition>> {
    // Start from where the scripted approach actually left the windows, not
    // from plan targets, so the first tick never snaps anything backwards.
    let positions = players
        .iter()
        .map(|player| read_window_position(app, &player.instance_id))
        .collect::<Option<Vec<_>>>()?;
    if let Some(window) = app.get_webview_window(prop_label) {
        if let (Ok(position), Ok(scale)) = (window.outer_position(), window.scale_factor()) {
            let logical: LogicalPosition<f64> = position.to_logical(scale);
            // Ball centre = prop window top-left + half of the 72x72 window.
            let ball = (logical.x + 36.0, logical.y + 36.0);
            let (x, y) = pitch.clamp_point(ball.0, ball.1);
            return run_match_inner(
                app,
                scene_id,
                players,
                prop_label,
                Ball {
                    x,
                    y,
                    vx: 0.0,
                    vy: 0.0,
                },
                positions,
                pitch,
                player_area,
                duration_ms,
                cancel,
            )
            .await;
        }
    }
    run_match_inner(
        app,
        scene_id,
        players,
        prop_label,
        Ball {
            x: ball_center.0,
            y: ball_center.1,
            vx: 0.0,
            vy: 0.0,
        },
        positions,
        pitch,
        player_area,
        duration_ms,
        cancel,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_match_inner(
    app: &AppHandle,
    scene_id: &str,
    players: &[FootballPlayer],
    prop_label: &str,
    mut ball: Ball,
    mut positions: Vec<PetPosition>,
    pitch: PitchBounds,
    player_area: PitchBounds,
    duration_ms: u64,
    cancel: &AtomicBool,
) -> Option<Vec<PetPosition>> {
    let mut cooldowns = vec![0.0f64; players.len()];
    let mut last_kicker: Option<usize> = None;
    let started = Instant::now();
    let mut last_phase_ms = 0u64;
    let mut last_say_ms = 0u64;
    let mut possession: Option<usize> = None;
    let mut dribble_touches = vec![0u32; players.len()];
    let mut velocities = vec![(0.0f64, 0.0f64); players.len()];

    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let elapsed = started.elapsed().as_millis() as u64;
        if elapsed >= duration_ms {
            break;
        }
        for cooldown in cooldowns.iter_mut() {
            *cooldown = (*cooldown - TICK_S).max(0.0);
        }

        ball.step(&pitch);

        steer_players(players, &mut positions, &ball, &mut velocities);
        separate_players(&mut positions, players);
        clamp_players(&mut positions, players, &player_area);
        separate_players(&mut positions, players);

        // Contact resolution, one touch per tick: a pet without possession
        // tackles, a charged carrier shoots, everything else dribbles.
        for (index, player) in players.iter().enumerate() {
            if cooldowns[index] > 0.0 {
                continue;
            }
            let center = (
                positions[index].x + player.width / 2.0,
                positions[index].y + player.height / 2.0,
            );
            if !touches_ball(
                (positions[index].x, positions[index].y),
                player.width,
                player.height,
                (ball.x, ball.y),
            ) {
                continue;
            }
            let kind = classify_touch(
                index,
                possession,
                dribble_touches[index],
                rand::rng().random_range(0.0..1.0),
            );
            let notable = kind != TouchKind::Dribble;
            let (vx, vy, escape, cooldown) = match kind {
                TouchKind::Dribble => {
                    dribble_touches[index] += 1;
                    let (vx, vy) =
                        dribble_push(velocities[index], center, (ball.x, ball.y));
                    (vx, vy, DRIBBLE_ESCAPE, DRIBBLE_COOLDOWN_S)
                }
                TouchKind::Shoot => {
                    dribble_touches[index] = 0;
                    let travel =
                        travel_direction(velocities[index], center, (ball.x, ball.y));
                    let (vx, vy) = shoot_velocity(travel);
                    (vx, vy, BALL_RADIUS + KICK_ESCAPE, KICK_COOLDOWN_S)
                }
                TouchKind::Tackle => {
                    dribble_touches[index] = 0;
                    let (vx, vy) =
                        kick_velocity(center, (ball.x, ball.y), (ball.vx, ball.vy));
                    (vx, vy, BALL_RADIUS + KICK_ESCAPE, KICK_COOLDOWN_S)
                }
            };
            possession = Some(index);
            if notable {
                last_kicker = Some(index);
            }
            ball.vx = vx;
            ball.vy = vy;
            let kick_distance = ball.vx.hypot(ball.vy).max(1.0);
            ball.x += ball.vx / kick_distance * escape;
            ball.y += ball.vy / kick_distance * escape;
            let (x, y) = pitch.clamp_point(ball.x, ball.y);
            ball.x = x;
            ball.y = y;
            cooldowns[index] = cooldown;
            if notable
                && elapsed - last_say_ms >= MATCH_SAY_COOLDOWN_MS
                && !player.say.is_empty()
                && rand::rng().random_range(0.0..1.0) < SAY_CHANCE
            {
                last_say_ms = elapsed;
                last_phase_ms = elapsed;
                emit_match_phase(
                    app,
                    scene_id,
                    players,
                    &positions,
                    &ball,
                    last_kicker,
                    Some(index),
                );
            }
            break;
        }

        for (index, player) in players.iter().enumerate() {
            if let Ok(label) = super::instance_label(&player.instance_id) {
                if let Some(window) = app.get_webview_window(&label) {
                    let _ = window
                        .set_position(LogicalPosition::new(positions[index].x, positions[index].y));
                    let _ = super::reposition_pet_speech(app, &player.instance_id);
                }
            }
        }
        if let Some(window) = app.get_webview_window(prop_label) {
            let _ = window.set_position(LogicalPosition::new(ball.x - 36.0, ball.y - 36.0));
        }

        if elapsed - last_phase_ms >= MATCH_PHASE_INTERVAL_MS {
            last_phase_ms = elapsed;
            emit_match_phase(app, scene_id, players, &positions, &ball, last_kicker, None);
        }

        tokio::time::sleep(std::time::Duration::from_millis(TICK_MS)).await;
    }

    Some(positions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pitch() -> PitchBounds {
        PitchBounds {
            min_x: 0.0,
            min_y: 0.0,
            max_x: 1000.0,
            max_y: 700.0,
        }
    }

    #[test]
    fn kick_pushes_ball_away_from_the_player() {
        let (vx, vy) = kick_velocity((0.0, 0.0), (40.0, 0.0), (0.0, 0.0));
        assert!(vx > 300.0, "expected a strong kick to the right, got {vx}");
        assert!(vy.abs() < vx * 0.25, "jitter must stay small, got {vy}");
    }

    #[test]
    fn kick_never_stalls_on_a_dead_ball() {
        let (vx, vy) = kick_velocity((0.0, 0.0), (0.0, 0.0), (0.0, 0.0));
        assert!(vx.hypot(vy) >= KICK_SPEED_MIN * 0.97);
    }

    #[test]
    fn ball_bounces_off_the_left_wall() {
        let mut ball = Ball {
            x: 1.0,
            y: 350.0,
            vx: -220.0,
            vy: 0.0,
        };
        assert!(ball.step(&pitch()));
        assert!(
            ball.vx > 0.0,
            "bounce must flip the velocity, got {}",
            ball.vx
        );
        assert_eq!(ball.x, 0.0);
    }

    #[test]
    fn friction_stops_the_ball_without_reversing_it() {
        let mut ball = Ball {
            x: 500.0,
            y: 350.0,
            vx: 80.0,
            vy: 0.0,
        };
        for _ in 0..40 {
            ball.step(&pitch());
        }
        assert_eq!(ball.vx, 0.0);
        assert_eq!(ball.vy, 0.0);
    }

    #[test]
    fn ball_touches_rect_by_closest_point() {
        // Ball just outside the right edge of a 100x200 rect.
        assert!(touches_ball(
            (0.0, 0.0),
            100.0,
            200.0,
            (100.0 + 10.0, 100.0)
        ));
        // Far beyond the corner radius.
        assert!(!touches_ball(
            (0.0, 0.0),
            100.0,
            200.0,
            (100.0 + 60.0, 200.0 + 60.0)
        ));
    }

    #[test]
    fn players_are_pushed_apart_not_swapped() {
        let players = vec![
            FootballPlayer {
                instance_id: "a".into(),
                pet_id: "a".into(),
                say: String::new(),
                width: 100.0,
                height: 100.0,
            },
            FootballPlayer {
                instance_id: "b".into(),
                pet_id: "b".into(),
                say: String::new(),
                width: 100.0,
                height: 100.0,
            },
        ];
        let mut positions = vec![PetPosition { x: 0.0, y: 0.0 }, PetPosition { x: 50.0, y: 0.0 }];
        separate_players(&mut positions, &players);
        let first_right = positions[0].x + 100.0;
        let second_left = positions[1].x;
        assert!(
            second_left >= first_right - 0.01,
            "windows must not overlap after separation: {positions:?}"
        );
    }

    #[test]
    fn dribble_push_is_gentle_and_forward() {
        let (vx, vy) = dribble_push((150.0, 0.0), (0.0, 0.0), (30.0, 0.0));
        assert!(vx > 100.0, "ball must roll ahead, got {vx}");
        assert!(
            vx <= 150.0 + DRIBBLE_PUSH_BONUS,
            "dribble must stay gentle, got {vx}"
        );
        assert!(vy.abs() < 1.0, "push must follow travel direction, got {vy}");
    }

    #[test]
    fn dribble_push_falls_back_to_ball_direction_when_stationary() {
        let (vx, vy) = dribble_push((0.0, 0.0), (0.0, 0.0), (30.0, 0.0));
        assert!((vx - DRIBBLE_MIN_PUSH).abs() < 0.01);
        assert!(vy.abs() < 0.01);
        // Ball exactly on the pet's centre: never stall, never reverse.
        let (vx, _) = dribble_push((0.0, 0.0), (0.0, 0.0), (0.0, 0.0));
        assert!(vx > 0.0);
    }

    #[test]
    fn possession_changes_make_touches_tackles() {
        assert_eq!(classify_touch(1, Some(0), 0, 1.0), TouchKind::Tackle);
        assert_eq!(classify_touch(0, Some(0), 0, 1.0), TouchKind::Dribble);
        assert_eq!(classify_touch(0, None, 0, 1.0), TouchKind::Dribble);
    }

    #[test]
    fn charged_carriers_shoot_after_enough_touches() {
        assert_eq!(
            classify_touch(0, Some(0), SHOOT_CHARGE_AFTER_TOUCHES - 1, 1.0),
            TouchKind::Shoot
        );
        assert_eq!(classify_touch(0, Some(0), 0, 0.0), TouchKind::Shoot);
    }

    #[test]
    fn shots_follow_the_travel_direction_with_tight_jitter() {
        for _ in 0..20 {
            let (vx, vy) = shoot_velocity((1.0, 0.0));
            assert!(vx >= KICK_SPEED_MIN * 0.99, "shot must fly forward: {vx}");
            assert!(
                vy.abs() <= KICK_SPEED_MAX * 0.13,
                "jitter must stay tight: {vy}"
            );
        }
    }

    #[test]
    fn steering_writes_each_pets_actual_velocity() {
        let players = vec![FootballPlayer {
            instance_id: "a".into(),
            pet_id: "a".into(),
            say: String::new(),
            width: 100.0,
            height: 100.0,
        }];
        let ball = Ball {
            x: 500.0,
            y: 0.0,
            vx: 0.0,
            vy: 0.0,
        };
        let mut positions = vec![PetPosition { x: 0.0, y: 0.0 }];
        let mut velocities = vec![(0.0, 0.0)];
        steer_players(&players, &mut positions, &ball, &mut velocities);
        let (vx, vy) = velocities[0];
        let speed = vx.hypot(vy);
        assert!(
            (speed - CHASER_SPEED).abs() < 0.5,
            "a charging pet must report its real speed, got ({vx}, {vy})"
        );
        assert!(vx > 0.0, "the pet must move toward the ball");
        assert!(positions[0].x > 0.0, "the pet must actually move");
    }
}
