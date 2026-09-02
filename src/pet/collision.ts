/**
 * Collision geometry for pet windows.
 *
 * Window positions are top-left logical coordinates.  Keeping this contract
 * in one place is important: treating a position as a window centre is what
 * used to make differently-scaled pets overlap during social scenes.
 */

export interface CollisionPoint {
  x: number;
  y: number;
}

export interface CollisionSize {
  width: number;
  height: number;
}

export interface CollisionRect extends CollisionPoint, CollisionSize {}

export interface CollisionBounds {
  minX: number;
  maxX: number;
  minY: number;
  maxY: number;
}

const GEOMETRY_EPSILON = 0.01;
const ROUTE_CLEARANCE = 2;

interface ForbiddenRect {
  left: number;
  right: number;
  top: number;
  bottom: number;
}

function forbiddenRect(
  obstacle: CollisionRect,
  ownSize: CollisionSize,
  gap: number,
): ForbiddenRect {
  // The point being planned is the top-left corner of our window.  Expanding
  // an obstacle by our full width/height (rather than half of them) converts
  // rectangle-vs-rectangle collision into point-vs-rectangle collision.
  return {
    left: obstacle.x - ownSize.width - gap,
    right: obstacle.x + obstacle.width + gap,
    top: obstacle.y - ownSize.height - gap,
    bottom: obstacle.y + obstacle.height + gap,
  };
}

function pointInside(point: CollisionPoint, rect: ForbiddenRect): boolean {
  return (
    point.x > rect.left + GEOMETRY_EPSILON &&
    point.x < rect.right - GEOMETRY_EPSILON &&
    point.y > rect.top + GEOMETRY_EPSILON &&
    point.y < rect.bottom - GEOMETRY_EPSILON
  );
}

/** Conservative segment/rectangle intersection using the slab method. */
function segmentHitsRect(start: CollisionPoint, end: CollisionPoint, rect: ForbiddenRect): boolean {
  const dx = end.x - start.x;
  const dy = end.y - start.y;
  let enter = 0;
  let leave = 1;

  const clip = (origin: number, delta: number, min: number, max: number): boolean => {
    if (Math.abs(delta) < GEOMETRY_EPSILON) {
      return origin > min && origin < max;
    }
    const first = (min - origin) / delta;
    const second = (max - origin) / delta;
    const nextEnter = Math.min(first, second);
    const nextLeave = Math.max(first, second);
    enter = Math.max(enter, nextEnter);
    leave = Math.min(leave, nextLeave);
    return enter < leave - GEOMETRY_EPSILON;
  };

  return clip(start.x, dx, rect.left, rect.right) && clip(start.y, dy, rect.top, rect.bottom);
}

function pointWithinBounds(point: CollisionPoint, bounds: CollisionBounds): boolean {
  return (
    point.x >= bounds.minX &&
    point.x <= bounds.maxX &&
    point.y >= bounds.minY &&
    point.y <= bounds.maxY
  );
}

function canStandAt(
  point: CollisionPoint,
  ownSize: CollisionSize,
  bounds: CollisionBounds,
  obstacles: CollisionRect[],
  gap: number,
): boolean {
  if (!pointWithinBounds(point, bounds)) return false;
  return obstacles.every((obstacle) => !pointInside(point, forbiddenRect(obstacle, ownSize, gap)));
}

function canTravel(
  start: CollisionPoint,
  end: CollisionPoint,
  forbidden: ForbiddenRect[],
): boolean {
  if (forbidden.some((rect) => pointInside(start, rect) || pointInside(end, rect))) return false;
  return forbidden.every((rect) => !segmentHitsRect(start, end, rect));
}

function distance(first: CollisionPoint, second: CollisionPoint): number {
  return Math.hypot(second.x - first.x, second.y - first.y);
}

function nearestSafeDestination(
  target: CollisionPoint,
  ownSize: CollisionSize,
  bounds: CollisionBounds,
  obstacles: CollisionRect[],
  gap: number,
): CollisionPoint | null {
  if (canStandAt(target, ownSize, bounds, obstacles, gap)) return target;

  const candidates: CollisionPoint[] = [];
  for (const obstacle of obstacles) {
    const rect = forbiddenRect(obstacle, ownSize, gap);
    candidates.push(
      { x: rect.left - ROUTE_CLEARANCE, y: target.y },
      { x: rect.right + ROUTE_CLEARANCE, y: target.y },
      { x: target.x, y: rect.top - ROUTE_CLEARANCE },
      { x: target.x, y: rect.bottom + ROUTE_CLEARANCE },
      { x: rect.left - ROUTE_CLEARANCE, y: rect.top - ROUTE_CLEARANCE },
      { x: rect.right + ROUTE_CLEARANCE, y: rect.top - ROUTE_CLEARANCE },
      { x: rect.left - ROUTE_CLEARANCE, y: rect.bottom + ROUTE_CLEARANCE },
      { x: rect.right + ROUTE_CLEARANCE, y: rect.bottom + ROUTE_CLEARANCE },
    );
  }

  return candidates
    .map((candidate) => ({
      x: Math.min(bounds.maxX, Math.max(bounds.minX, candidate.x)),
      y: Math.min(bounds.maxY, Math.max(bounds.minY, candidate.y)),
    }))
    .filter((candidate) => canStandAt(candidate, ownSize, bounds, obstacles, gap))
    .sort((left, right) => distance(left, target) - distance(right, target))[0] ?? null;
}

/**
 * Returns waypoints after `start`, including a safe replacement destination
 * when the requested target is inside another pet's collision volume.
 *
 * The visibility graph is deliberately small (four corners per obstacle), so
 * it remains cheap to recompute while another pet is moving.  This is more
 * reliable than a single perpendicular detour because it handles two or more
 * pets forming a corridor.
 */
export function planCollisionFreeRoute(
  start: CollisionPoint,
  target: CollisionPoint,
  ownSize: CollisionSize,
  bounds: CollisionBounds,
  obstacles: CollisionRect[],
  gap: number,
): CollisionPoint[] {
  const forbidden = obstacles.map((obstacle) => forbiddenRect(obstacle, ownSize, gap));
  const destination = nearestSafeDestination(target, ownSize, bounds, obstacles, gap);
  if (!destination || !canStandAt(start, ownSize, bounds, obstacles, gap)) return [];
  if (canTravel(start, destination, forbidden)) return [destination];

  const nodes: CollisionPoint[] = [start, destination];
  for (const rect of forbidden) {
    nodes.push(
      { x: rect.left - ROUTE_CLEARANCE, y: rect.top - ROUTE_CLEARANCE },
      { x: rect.right + ROUTE_CLEARANCE, y: rect.top - ROUTE_CLEARANCE },
      { x: rect.left - ROUTE_CLEARANCE, y: rect.bottom + ROUTE_CLEARANCE },
      { x: rect.right + ROUTE_CLEARANCE, y: rect.bottom + ROUTE_CLEARANCE },
    );
  }
  const usable = nodes.filter((node) => canStandAt(node, ownSize, bounds, obstacles, gap));
  const startIndex = usable.findIndex((node) => node === start);
  const destinationIndex = usable.findIndex((node) => node === destination);
  if (startIndex < 0 || destinationIndex < 0) return [];

  const costs = usable.map(() => Number.POSITIVE_INFINITY);
  const previous = usable.map(() => -1);
  const visited = usable.map(() => false);
  costs[startIndex] = 0;

  for (;;) {
    let current = -1;
    for (let index = 0; index < usable.length; index += 1) {
      if (!visited[index] && (current < 0 || costs[index] < costs[current])) current = index;
    }
    if (current < 0 || costs[current] === Number.POSITIVE_INFINITY) break;
    if (current === destinationIndex) break;
    visited[current] = true;
    for (let next = 0; next < usable.length; next += 1) {
      if (visited[next] || next === current || !canTravel(usable[current], usable[next], forbidden)) continue;
      const nextCost = costs[current] + distance(usable[current], usable[next]);
      if (nextCost < costs[next]) {
        costs[next] = nextCost;
        previous[next] = current;
      }
    }
  }

  if (costs[destinationIndex] === Number.POSITIVE_INFINITY) return [];
  const route: CollisionPoint[] = [];
  for (let index = destinationIndex; index >= 0; index = previous[index]) {
    route.unshift(usable[index]);
    if (index === startIndex) break;
  }
  return route[0] === start ? route.slice(1) : [];
}

export function isCollisionFree(
  point: CollisionPoint,
  ownSize: CollisionSize,
  obstacles: CollisionRect[],
  gap: number,
): boolean {
  return obstacles.every((obstacle) => !pointInside(point, forbiddenRect(obstacle, ownSize, gap)));
}
