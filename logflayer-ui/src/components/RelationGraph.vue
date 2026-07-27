<script setup lang="ts">
/**
 * Force-directed relation graph.
 * Dependency-free simulation: every node repels every other node (Coulomb-style),
 * edges act as springs, and a weak gravity keeps the graph centered. Nodes are
 * draggable; the canvas supports wheel-zoom, drag-to-pan, and zoom controls.
 * Emits `expand` so a parent can open it in a larger modal.
 */
import { ref, watch, onMounted, onBeforeUnmount, computed } from 'vue'
import {
  Plus, Minus, RotateCcw, Crosshair, Maximize2, ExternalLink, X, Copy, Check, CircleDot, Waypoints,
  ArrowUpLeft, ArrowDownRight, Route,
} from 'lucide-vue-next'
import type { RelationEdge, GraphNode } from '../types'
import { isActor } from '../types'

const props = withDefaults(
  defineProps<{
    relations: RelationEdge[]
    /// Events *and* actors — a traversal returns both since Stage 12.
    entities: GraphNode[]
    expanded?: boolean
  }>(),
  { expanded: false },
)
const emit = defineEmits<{
  expand: []
  detach: []
  /** Ask the parent to run a server-side traversal from this entity id.
   *  The component stays presentational — it never fetches. */
  'traverse-downstream': [entityId: string]
  'traverse-upstream': [entityId: string]
  /** Ask the parent to resolve the shortest path between two picked nodes. */
  'find-path': [from: string, to: string]
}>()

// ── Coordinate space (SVG scales to container width via viewBox) ──────────────
const W = 820
const H = 480
const CX = W / 2
const CY = H / 2
const R = 20 // node radius

interface SimNode {
  id: string
  label: string
  type: string
  x: number
  y: number
  vx: number
  vy: number
  fixed: boolean // temporarily held while dragging
  pinned: boolean // permanently anchored via triple-click
}
interface SimLink {
  source: number
  target: number
  type: string
}

const nodes = ref<SimNode[]>([])
const links = ref<SimLink[]>([])

// ── Colors: stable hue per entity_type ────────────────────────────────────────
const PALETTE = ['#dc143c', '#00d4ff', '#22c55e', '#f59e0b', '#a855f7', '#3b82f6', '#ec4899', '#14b8a6']
function colorFor(type: string): string {
  let h = 0
  for (let i = 0; i < type.length; i++) h = (h * 31 + type.charCodeAt(i)) >>> 0
  return PALETTE[h % PALETTE.length]!
}

// A graph node is either a log event or one of its participants (Stage 12).
// Events are keyed on `entity_id`, actors on `actor_id` — looking up only the
// former would leave every agent, skill and resource unlabelled and grey.
function nodeFor(id: string): GraphNode | undefined {
  return props.entities.find((x) =>
    isActor(x) ? x.actor_id === id : x.entity_id === id,
  )
}

function labelFor(id: string): string {
  const n = nodeFor(id)
  if (!n) return id.slice(0, 6)
  // An actor's name is its identity; an event's is its tool or type.
  const label = isActor(n) ? n.name : (n.tool_name ?? n.entity_type ?? id)
  return label.toString().slice(0, 14)
}

/// Drives the node colour, so actors get their own hues rather than sharing the
/// event palette.
function typeFor(id: string): string {
  const n = nodeFor(id)
  if (!n) return 'unknown'
  return isActor(n) ? `actor:${n.kind}` : n.entity_type
}

// ── Build graph from props ────────────────────────────────────────────────────
function build() {
  const idList: string[] = []
  const index = new Map<string, number>()
  const add = (id: string) => {
    if (!index.has(id)) {
      index.set(id, idList.length)
      idList.push(id)
    }
    return index.get(id)!
  }

  const newLinks: SimLink[] = []
  for (const r of props.relations) {
    const s = add(r.source_entity_id)
    const t = add(r.target_entity_id)
    newLinks.push({ source: s, target: t, type: r.relation_type })
  }

  const n = idList.length
  nodes.value = idList.map((id, i) => {
    const angle = (i / Math.max(n, 1)) * Math.PI * 2
    return {
      id,
      label: labelFor(id),
      type: typeFor(id),
      x: CX + Math.cos(angle) * 150 + (Math.random() - 0.5) * 20,
      y: CY + Math.sin(angle) * 150 + (Math.random() - 0.5) * 20,
      vx: 0,
      vy: 0,
      fixed: false,
      pinned: false,
    }
  })
  links.value = newLinks

  // Any open detail window refers to the *previous* arrays. `edgeIndex` is a
  // positional index, so leaving it set would silently retarget the window at
  // whatever relation now occupies that slot — very reachable now that a
  // traversal swaps `props.relations` wholesale on a click. Drop a node
  // selection too if that node is no longer in the graph.
  edgeIndex.value = null
  if (detailId.value && !index.has(detailId.value)) {
    detailId.value = null
  }
  // Same for a half-finished path pick: if the source node is gone (the relation
  // filter changed, or a different sample was selected), its cyan ring would
  // vanish while the instruction strip still asked for a target — and the next
  // click would emit a path from an invisible node.
  if (pathFrom.value && !index.has(pathFrom.value)) {
    pathFrom.value = null
  }

  reheat()
}

// ── Simulation ────────────────────────────────────────────────────────────────
const REPEL = 5200
const LINK_DIST = 120
const LINK_K = 0.028
const CENTER_K = 0.01
const DAMPING = 0.85
const MAX_V = 18

let alpha = 0
let raf = 0

function reheat() {
  alpha = 1
  if (!raf) raf = requestAnimationFrame(tick)
}

function tick() {
  const ns = nodes.value
  const ls = links.value
  const n = ns.length

  for (let i = 0; i < n; i++) {
    const a = ns[i]!
    for (let j = i + 1; j < n; j++) {
      const b = ns[j]!
      let dx = a.x - b.x
      let dy = a.y - b.y
      let d2 = dx * dx + dy * dy
      if (d2 < 0.01) {
        dx = Math.random() - 0.5
        dy = Math.random() - 0.5
        d2 = 0.01
      }
      const d = Math.sqrt(d2)
      const f = (REPEL / d2) * alpha
      a.vx += (dx / d) * f
      a.vy += (dy / d) * f
      b.vx -= (dx / d) * f
      b.vy -= (dy / d) * f
    }
  }

  for (const l of ls) {
    const a = ns[l.source]!
    const b = ns[l.target]!
    const dx = b.x - a.x
    const dy = b.y - a.y
    const d = Math.sqrt(dx * dx + dy * dy) || 0.01
    const f = (d - LINK_DIST) * LINK_K * alpha
    a.vx += (dx / d) * f
    a.vy += (dy / d) * f
    b.vx -= (dx / d) * f
    b.vy -= (dy / d) * f
  }

  let energy = 0
  for (const nd of ns) {
    if (nd.fixed || nd.pinned) {
      nd.vx = 0
      nd.vy = 0
      continue
    }
    nd.vx += (CX - nd.x) * CENTER_K * alpha
    nd.vy += (CY - nd.y) * CENTER_K * alpha
    nd.vx = Math.max(-MAX_V, Math.min(MAX_V, nd.vx)) * DAMPING
    nd.vy = Math.max(-MAX_V, Math.min(MAX_V, nd.vy)) * DAMPING
    nd.x += nd.vx
    nd.y += nd.vy
    nd.x = Math.max(R + 2, Math.min(W - R - 2, nd.x))
    nd.y = Math.max(R + 2, Math.min(H - R - 2, nd.y))
    energy += nd.vx * nd.vx + nd.vy * nd.vy
  }

  alpha *= 0.992
  if (alpha > 0.02 && energy > 0.05) {
    raf = requestAnimationFrame(tick)
  } else {
    raf = 0
  }
}

// ── Zoom + pan (transform on inner group) ─────────────────────────────────────
const scale = ref(1)
const tx = ref(0)
const ty = ref(0)
const MIN_SCALE = 0.3
const MAX_SCALE = 4

const svgRef = ref<SVGSVGElement | null>(null)

/** Screen coords → viewBox (user) coords. */
function toViewBox(evt: PointerEvent | WheelEvent): { x: number; y: number } {
  const svg = svgRef.value
  if (!svg) return { x: 0, y: 0 }
  const pt = svg.createSVGPoint()
  pt.x = evt.clientX
  pt.y = evt.clientY
  const ctm = svg.getScreenCTM()
  if (!ctm) return { x: 0, y: 0 }
  const p = pt.matrixTransform(ctm.inverse())
  return { x: p.x, y: p.y }
}
/** viewBox coords → graph (pre-transform) coords. */
function toGraph(vx: number, vy: number) {
  return { x: (vx - tx.value) / scale.value, y: (vy - ty.value) / scale.value }
}

function zoomAround(vx: number, vy: number, factor: number) {
  const g = toGraph(vx, vy)
  const s = Math.max(MIN_SCALE, Math.min(MAX_SCALE, scale.value * factor))
  scale.value = s
  tx.value = vx - g.x * s
  ty.value = vy - g.y * s
}
function zoomIn() {
  zoomAround(CX, CY, 1.25)
}
function zoomOut() {
  zoomAround(CX, CY, 0.8)
}
function resetView() {
  scale.value = 1
  tx.value = 0
  ty.value = 0
}
/** Frame all nodes: scale to fit and pan so the graph sits centered in view. */
function centerView() {
  const ns = nodes.value
  if (!ns.length) return
  let minX = Infinity
  let minY = Infinity
  let maxX = -Infinity
  let maxY = -Infinity
  for (const n of ns) {
    minX = Math.min(minX, n.x - R)
    minY = Math.min(minY, n.y - R)
    maxX = Math.max(maxX, n.x + R)
    maxY = Math.max(maxY, n.y + R)
  }
  const bw = Math.max(maxX - minX, 1)
  const bh = Math.max(maxY - minY, 1)
  const pad = 48
  const s = Math.max(
    MIN_SCALE,
    Math.min(MAX_SCALE, Math.min((W - pad * 2) / bw, (H - pad * 2) / bh)),
  )
  const cx = (minX + maxX) / 2
  const cy = (minY + maxY) / 2
  scale.value = s
  tx.value = W / 2 - cx * s
  ty.value = H / 2 - cy * s
}
function onWheel(evt: WheelEvent) {
  const v = toViewBox(evt)
  zoomAround(v.x, v.y, evt.deltaY < 0 ? 1.12 : 0.89)
}

// ── Pointer: node drag OR canvas pan ──────────────────────────────────────────
let dragIndex = -1
let panning = false
let panLast = { x: 0, y: 0 }

// Click gestures on a node (a press only counts as a click if it barely moved):
//   double-click → open the node's data window
//   triple-click → pin / unpin
// Both are resolved after a short settle delay so a triple isn't mistaken for a
// double on the way up.
let downInfo: { index: number; x: number; y: number; t: number } | null = null
let clickTrack = { index: -1, count: 0 }
let resolveTimer = 0
const CLICK_MOVE_PX = 5
const CLICK_MAX_MS = 400
const RESOLVE_MS = 280

const detailId = ref<string | null>(null)
const detailNode = computed<GraphNode | null>(() =>
  detailId.value ? nodeFor(detailId.value) ?? null : null,
)
/// The selected node when it is a log event. `null` for actors, which have a
/// different shape and their own rows below.
const detailEntity = computed(() => {
  const n = detailNode.value
  return n && !isActor(n) ? n : null
})
/// The selected node when it is an actor.
const detailActor = computed(() => {
  const n = detailNode.value
  return n && isActor(n) ? n : null
})
// Border color of the data window = the selected node's border color.
const detailColor = computed(() => {
  const nd = detailId.value ? nodes.value.find((n) => n.id === detailId.value) : null
  return nd ? colorFor(nd.type) : '#dc143c'
})

// ── Edge (relation) detail — double-click an edge ─────────────────────────────
// links[i] is built 1:1 from props.relations[i], so the index maps straight back.
const edgeIndex = ref<number | null>(null)
const edgeDetail = computed(() =>
  edgeIndex.value != null ? props.relations[edgeIndex.value] ?? null : null,
)
const edgeColor = computed(() =>
  edgeDetail.value ? colorFor(edgeDetail.value.relation_type) : '#dc143c',
)
function openEdge(i: number) {
  edgeIndex.value = i
}
function closeEdge() {
  edgeIndex.value = null
}
const edgeRows = computed(() => {
  const e = edgeDetail.value
  if (!e) return [] as { label: string; value: string }[]
  return [
    { label: 'Type', value: e.relation_type },
    { label: 'Source', value: labelFor(e.source_entity_id) },
    { label: 'Target', value: labelFor(e.target_entity_id) },
    { label: 'Confidence', value: `${Math.round(e.confidence * 100)}%` },
    { label: 'Origin', value: e.source },
    { label: 'Created', value: e.created_at },
    { label: 'Relation ID', value: e.relation_id },
  ]
})
function closeDetail() {
  detailId.value = null
}

// Copy-to-clipboard for any field value, with a transient ✓ on the source.
const copiedKey = ref<string | null>(null)
async function copyValue(text: string, key: string) {
  try {
    await navigator.clipboard.writeText(text)
  } catch {
    // Fallback for non-secure contexts.
    const ta = document.createElement('textarea')
    ta.value = text
    ta.style.position = 'fixed'
    ta.style.opacity = '0'
    document.body.appendChild(ta)
    ta.select()
    try {
      document.execCommand('copy')
    } catch {
      /* ignore */
    }
    document.body.removeChild(ta)
  }
  copiedKey.value = key
  window.setTimeout(() => {
    if (copiedKey.value === key) copiedKey.value = null
  }, 1200)
}

function registerClick(i: number) {
  if (clickTrack.index !== i) {
    // Switching to a different node ends the previous node's gesture. Resolve it
    // now rather than discarding it: in path mode the user clicks source then
    // target in quick succession, and dropping the first click would silently
    // make the *target* the source.
    if (resolveTimer) {
      clearTimeout(resolveTimer)
      resolveTimer = 0
      resolveClicks()
    }
    clickTrack = { index: i, count: 0 }
  }
  clickTrack.count += 1
  if (resolveTimer) clearTimeout(resolveTimer)
  resolveTimer = window.setTimeout(resolveClicks, RESOLVE_MS)
}

// ── Path picking ──────────────────────────────────────────────────────────────
// Shortest-path lookup needs two nodes, and there is no natural gesture for
// "these two". Rather than inventing one, path mode is an explicit toggle: while
// it is on, a single click picks first the source then the target, and the
// component emits. Single-click is otherwise unused on nodes — double opens the
// detail window, triple pins — so nothing is overloaded.
const pathMode = ref(false)
const pathFrom = ref<string | null>(null)

function togglePathMode() {
  pathMode.value = !pathMode.value
  pathFrom.value = null
  // The detail window would sit on top of the nodes being picked.
  if (pathMode.value) detailId.value = null
}

function pickPathNode(id: string) {
  if (pathFrom.value === null) {
    pathFrom.value = id
    return
  }
  if (pathFrom.value === id) {
    // Clicking the same node again is a deselect, not a zero-length path query.
    pathFrom.value = null
    return
  }
  emit('find-path', pathFrom.value, id)
  pathFrom.value = null
  pathMode.value = false
}

function resolveClicks() {
  const { index, count } = clickTrack
  clickTrack = { index: -1, count: 0 }
  resolveTimer = 0
  const nd = nodes.value[index]
  if (!nd) return
  if (pathMode.value && count <= 2) {
    // In path mode a double-click is a fumbled pick, not a request for the
    // detail window — opening it would cover the nodes still being picked,
    // which is why `togglePathMode` closes it on entry.
    pickPathNode(nd.id)
  } else if (count === 2) {
    detailId.value = nd.id
  } else if (count >= 3) {
    nd.pinned = !nd.pinned
    nd.vx = 0
    nd.vy = 0
    reheat()
  }
}

function onNodePointerDown(i: number, evt: PointerEvent) {
  evt.stopPropagation()
  dragIndex = i
  const nd = nodes.value[i]
  if (nd) nd.fixed = true
  downInfo = { index: i, x: evt.clientX, y: evt.clientY, t: performance.now() }
  ;(evt.target as Element).setPointerCapture?.(evt.pointerId)
  reheat()
}
function onBgPointerDown(evt: PointerEvent) {
  panning = true
  panLast = toViewBox(evt)
  ;(evt.currentTarget as Element).setPointerCapture?.(evt.pointerId)
}
function onPointerMove(evt: PointerEvent) {
  if (dragIndex >= 0) {
    const nd = nodes.value[dragIndex]
    if (!nd) return
    const v = toViewBox(evt)
    const g = toGraph(v.x, v.y)
    nd.x = Math.max(R + 2, Math.min(W - R - 2, g.x))
    nd.y = Math.max(R + 2, Math.min(H - R - 2, g.y))
    reheat()
  } else if (panning) {
    const v = toViewBox(evt)
    tx.value += v.x - panLast.x
    ty.value += v.y - panLast.y
    panLast = v
  }
}
function onPointerUp(evt?: PointerEvent) {
  if (dragIndex >= 0) {
    const nd = nodes.value[dragIndex]
    if (nd) nd.fixed = false // pinned nodes stay anchored via the tick() check
    // A short, near-stationary press counts as a click (drags don't).
    if (downInfo && evt) {
      const moved = Math.hypot(evt.clientX - downInfo.x, evt.clientY - downInfo.y)
      const held = performance.now() - downInfo.t
      if (moved < CLICK_MOVE_PX && held < CLICK_MAX_MS) registerClick(downInfo.index)
    }
    downInfo = null
    dragIndex = -1
    reheat()
  }
  panning = false
}

function edge(l: SimLink) {
  const a = nodes.value[l.source]!
  const b = nodes.value[l.target]!
  const dx = b.x - a.x
  const dy = b.y - a.y
  const d = Math.sqrt(dx * dx + dy * dy) || 1
  const ux = dx / d
  const uy = dy / d
  const x2 = b.x - ux * (R + 6)
  const y2 = b.y - uy * (R + 6)
  // Manual arrowhead so it matches the (per-type) edge color.
  const ah = 9
  const aw = 4.2
  const bx = x2 - ux * ah
  const by = y2 - uy * ah
  const arrow = `${x2},${y2} ${bx - uy * aw},${by + ux * aw} ${bx + uy * aw},${by - ux * aw}`
  return {
    x1: a.x + ux * R,
    y1: a.y + uy * R,
    x2,
    y2,
    mx: (a.x + b.x) / 2,
    my: (a.y + b.y) / 2,
    type: l.type,
    color: colorFor(l.type),
    arrow,
  }
}
const edges = computed(() => links.value.map(edge))

watch(() => props.relations, build, { deep: false })
onMounted(build)
onBeforeUnmount(() => {
  if (raf) cancelAnimationFrame(raf)
  if (resolveTimer) clearTimeout(resolveTimer)
})

// Fields shown in the node data window, in order.
//
// Actors and events carry entirely different information, so each gets its own
// row set rather than one union with most fields left blank.
const detailRows = computed(() => {
  const a = detailActor.value
  if (a) {
    return [
      { label: 'Kind', value: a.kind },
      { label: 'Name', value: a.name },
      { label: 'From field', value: a.source_field },
      { label: 'Events', value: String(a.event_count) },
      { label: 'Samples', value: String(a.sample_hashes.length) },
      { label: 'Tasks', value: String(a.task_ids.length) },
      { label: 'Actor ID', value: a.actor_id },
    ]
  }

  const e = detailEntity.value
  if (!e) return [] as { label: string; value: string }[]
  const rows: { label: string; value: string }[] = [
    { label: 'Type', value: e.entity_type },
    { label: 'Role', value: e.semantic_role },
  ]
  if (e.tool_name) rows.push({ label: 'Tool', value: e.tool_name })
  if (e.model_id) rows.push({ label: 'Model', value: e.model_id })
  if (e.mcp_server_id) rows.push({ label: 'MCP server', value: e.mcp_server_id })
  rows.push({ label: 'Line', value: String(e.line_index) })
  if (e.token_count != null) rows.push({ label: 'Tokens', value: String(e.token_count) })
  if (e.latency_ms != null) rows.push({ label: 'Latency', value: `${e.latency_ms} ms` })
  if (e.timestamp_utc) rows.push({ label: 'Timestamp', value: e.timestamp_utc })
  rows.push({ label: 'Span', value: e.span_id })
  rows.push({ label: 'Trace', value: e.trace_id })
  rows.push({ label: 'Entity ID', value: e.entity_id })
  return rows
})
</script>

<template>
  <div class="relative w-full" :class="expanded ? 'h-full' : ''">
    <svg
      ref="svgRef"
      :viewBox="`0 0 ${W} ${H}`"
      class="w-full select-none touch-none block"
      :class="expanded ? 'h-full' : 'h-auto'"
      :style="expanded ? '' : 'max-height: 520px'"
      @pointerdown="onBgPointerDown"
      @pointermove="onPointerMove"
      @pointerup="onPointerUp($event)"
      @pointerleave="onPointerUp($event)"
      @wheel.prevent="onWheel"
    >
      <defs>
        <marker id="rg-arrow" markerWidth="7" markerHeight="7" refX="6" refY="3.5" orient="auto">
          <path d="M0,0 L0,7 L7,3.5 z" fill="rgba(220,20,60,0.65)" />
        </marker>
      </defs>

      <g :transform="`translate(${tx}, ${ty}) scale(${scale})`">
        <!-- Edges -->
        <g>
          <g v-for="(e, i) in edges" :key="`e-${i}`">
            <line
              :x1="e.x1"
              :y1="e.y1"
              :x2="e.x2"
              :y2="e.y2"
              :stroke="e.color"
              stroke-opacity="0.55"
              stroke-width="1.6"
            />
            <polygon :points="e.arrow" :fill="e.color" fill-opacity="0.75" />
            <text
              :x="e.mx"
              :y="e.my - 4"
              text-anchor="middle"
              font-size="8"
              :fill="e.color"
              fill-opacity="0.9"
              class="pointer-events-none"
            >{{ e.type }}</text>
            <!-- transparent hit area: double-click to inspect the edge -->
            <line
              :x1="e.x1"
              :y1="e.y1"
              :x2="e.x2"
              :y2="e.y2"
              stroke="transparent"
              stroke-width="12"
              class="cursor-pointer"
              @pointerdown.stop
              @dblclick="openEdge(i)"
            />
          </g>
        </g>

        <!-- Nodes -->
        <g
          v-for="(nd, i) in nodes"
          :key="nd.id"
          class="cursor-grab active:cursor-grabbing"
          @pointerdown="onNodePointerDown(i, $event)"
        >
          <!-- Path-source ring: marks the node picked as the path's origin -->
          <circle
            v-if="pathFrom === nd.id"
            :cx="nd.x"
            :cy="nd.y"
            :r="R + 6"
            fill="none"
            stroke="#00d4ff"
            stroke-width="2"
            class="pointer-events-none"
          />
          <!-- Pin ring -->
          <circle
            v-if="nd.pinned"
            :cx="nd.x"
            :cy="nd.y"
            :r="R + 4"
            fill="none"
            stroke="#f59e0b"
            stroke-width="1.5"
            stroke-dasharray="3 3"
            class="pointer-events-none"
          />
          <circle
            :cx="nd.x"
            :cy="nd.y"
            :r="R"
            :fill="colorFor(nd.type) + '22'"
            :stroke="colorFor(nd.type)"
            :stroke-width="nd.pinned ? 2.5 : 1.5"
          />
          <text
            :x="nd.x"
            :y="nd.y + 3"
            text-anchor="middle"
            font-size="8"
            fill="rgba(245,245,220,0.85)"
            class="pointer-events-none"
          >{{ nd.label }}</text>
          <!-- Pin marker -->
          <circle
            v-if="nd.pinned"
            :cx="nd.x + R * 0.72"
            :cy="nd.y - R * 0.72"
            r="4.5"
            fill="#f59e0b"
            stroke="#0a0a0a"
            stroke-width="1"
            class="pointer-events-none"
          />
        </g>
      </g>
    </svg>

    <!-- Zoom controls -->
    <div class="absolute top-2 right-2 flex flex-col gap-1">
      <button
        class="w-7 h-7 flex items-center justify-center rounded bg-[#1a1a1a]/90 border border-[#333] text-[rgba(245,245,220,0.7)] hover:text-[#f5f5dc] hover:border-[#dc143c]/50 transition-colors"
        title="Zoom in"
        @click="zoomIn"
      >
        <Plus :size="15" />
      </button>
      <button
        class="w-7 h-7 flex items-center justify-center rounded bg-[#1a1a1a]/90 border border-[#333] text-[rgba(245,245,220,0.7)] hover:text-[#f5f5dc] hover:border-[#dc143c]/50 transition-colors"
        title="Zoom out"
        @click="zoomOut"
      >
        <Minus :size="15" />
      </button>
      <button
        class="w-7 h-7 flex items-center justify-center rounded bg-[#1a1a1a]/90 border border-[#333] text-[rgba(245,245,220,0.7)] hover:text-[#f5f5dc] hover:border-[#dc143c]/50 transition-colors"
        title="Center &amp; fit graph"
        @click="centerView"
      >
        <Crosshair :size="14" />
      </button>
      <button
        class="w-7 h-7 flex items-center justify-center rounded bg-[#1a1a1a]/90 border border-[#333] text-[rgba(245,245,220,0.7)] hover:text-[#f5f5dc] hover:border-[#dc143c]/50 transition-colors"
        title="Reset view (100%)"
        @click="resetView"
      >
        <RotateCcw :size="14" />
      </button>
      <button
        v-if="!expanded"
        class="w-7 h-7 flex items-center justify-center rounded bg-[#1a1a1a]/90 border border-[#333] text-[rgba(245,245,220,0.7)] hover:text-[#00d4ff] hover:border-[#00d4ff]/50 transition-colors"
        title="Expand in a modal"
        @click="emit('expand')"
      >
        <Maximize2 :size="14" />
      </button>
      <button
        class="w-7 h-7 flex items-center justify-center rounded border transition-colors"
        :class="pathMode
          ? 'bg-[#00d4ff]/25 border-[#00d4ff] text-[#00d4ff]'
          : 'bg-[#1a1a1a]/90 border-[#333] text-[rgba(245,245,220,0.7)] hover:text-[#00d4ff] hover:border-[#00d4ff]/50'"
        :title="pathMode ? 'Cancel path mode' : 'Find shortest path between two nodes'"
        @click="togglePathMode"
      >
        <Route :size="14" />
      </button>
      <button
        v-if="!expanded"
        class="w-7 h-7 flex items-center justify-center rounded bg-[#1a1a1a]/90 border border-[#333] text-[rgba(245,245,220,0.7)] hover:text-[#00d4ff] hover:border-[#00d4ff]/50 transition-colors"
        title="Detach to a separate window"
        @click="emit('detach')"
      >
        <ExternalLink :size="14" />
      </button>
    </div>

    <!-- Path-mode instruction strip -->
    <div
      v-if="pathMode"
      class="absolute top-2 left-1/2 -translate-x-1/2 flex items-center gap-2 px-3 py-1.5 rounded-full
             bg-[#00d4ff]/15 border border-[#00d4ff]/50 text-[11px] text-[#00d4ff] backdrop-blur-sm"
    >
      <Route :size="13" />
      <span v-if="pathFrom === null">Click the <strong>source</strong> node</span>
      <span v-else>Now click the <strong>target</strong> node</span>
      <button class="opacity-70 hover:opacity-100" title="Cancel" @click="togglePathMode">
        <X :size="12" />
      </button>
    </div>

    <!-- Zoom level readout -->
    <div class="absolute bottom-2 left-2 text-[10px] font-mono text-[rgba(245,245,220,0.35)]">
      {{ Math.round(scale * 100) }}%
    </div>

    <!-- Node data window (double-click a node) -->
    <div
      v-if="detailId"
      class="absolute top-2 left-2 w-72 max-w-[calc(100%-1rem)] max-h-[calc(100%-1rem)] overflow-y-auto rounded-lg bg-[#0f0f0f]/97 border shadow-2xl"
      :style="{ borderColor: detailColor }"
    >
      <div
        class="flex items-center justify-between gap-2 px-3 py-2 border-b sticky top-0 bg-[#0f0f0f]"
        :style="{ borderColor: detailColor + '33' }"
      >
        <div class="flex items-center gap-1.5 min-w-0">
          <CircleDot :size="13" class="shrink-0" :style="{ color: detailColor }" />
          <span class="text-[10px] uppercase tracking-wide text-[rgba(245,245,220,0.4)] shrink-0">
            {{ detailActor ? detailActor.kind : 'Node' }}
          </span>
          <span class="text-xs font-semibold text-[#f5f5dc] truncate">
            {{ detailActor?.name ?? detailEntity?.tool_name ?? detailEntity?.entity_type ?? '—' }}
          </span>
        </div>
        <button class="text-[rgba(245,245,220,0.45)] hover:text-[#f5f5dc] shrink-0" title="Close" @click="closeDetail">
          <X :size="14" />
        </button>
      </div>

      <!-- Traversal actions — ask the server to follow this node's edges beyond
           the currently-loaded sample. -->
      <div class="flex items-center gap-1 px-3 py-2 border-b border-[#222]">
        <button
          class="flex-1 inline-flex items-center justify-center gap-1 px-2 py-1 rounded text-[11px]
                 bg-[#00d4ff]/10 border border-[#00d4ff]/30 text-[#00d4ff]
                 hover:bg-[#00d4ff]/20 transition-colors"
          title="Follow inbound edges — what led to this entity"
          @click="detailId && emit('traverse-upstream', detailId)"
        >
          <ArrowUpLeft :size="12" />Upstream
        </button>
        <button
          class="flex-1 inline-flex items-center justify-center gap-1 px-2 py-1 rounded text-[11px]
                 bg-[#dc143c]/10 border border-[#dc143c]/30 text-[#ff6b8a]
                 hover:bg-[#dc143c]/20 transition-colors"
          title="Follow outbound edges — what this entity led to"
          @click="detailId && emit('traverse-downstream', detailId)"
        >
          Downstream<ArrowDownRight :size="12" />
        </button>
      </div>

      <!-- `detailRows` covers both kinds; the event-only blocks below are guarded
           separately, since an actor has no raw_text or extracted_fields. -->
      <div v-if="detailEntity || detailActor" class="p-3 space-y-2">
        <dl class="space-y-1.5">
          <div v-for="row in detailRows" :key="row.label" class="flex gap-2 text-xs items-start">
            <dt class="text-[rgba(245,245,220,0.40)] w-20 shrink-0">{{ row.label }}</dt>
            <dd
              class="text-[rgba(245,245,220,0.85)] font-mono break-all flex-1"
              :class="{ 'copy-blink': copiedKey === `f:${row.label}` }"
            >{{ row.value }}</dd>
            <button
              class="shrink-0 text-[rgba(245,245,220,0.3)] hover:text-[#00d4ff] transition-colors mt-0.5"
              :title="copiedKey === `f:${row.label}` ? 'Copied' : 'Copy value'"
              @click="copyValue(row.value, `f:${row.label}`)"
            >
              <component :is="copiedKey === `f:${row.label}` ? Check : Copy" :size="12" />
            </button>
          </div>
        </dl>

        <!-- Event-only: an actor has no originating log line or fields. -->
        <template v-if="detailEntity">
        <div v-if="detailEntity.raw_text">
          <div class="flex items-center justify-between mb-1">
            <div class="text-[rgba(245,245,220,0.40)] text-xs">Raw text</div>
            <button
              class="text-[rgba(245,245,220,0.3)] hover:text-[#00d4ff] transition-colors"
              :title="copiedKey === 'raw' ? 'Copied' : 'Copy raw text'"
              @click="copyValue(detailEntity.raw_text, 'raw')"
            >
              <component :is="copiedKey === 'raw' ? Check : Copy" :size="12" />
            </button>
          </div>
          <pre
            class="text-[10px] leading-snug text-[rgba(245,245,220,0.7)] bg-[#0a0a0a] rounded p-2 whitespace-pre-wrap break-all max-h-32 overflow-y-auto"
            :class="{ 'copy-blink': copiedKey === 'raw' }"
          >{{ detailEntity.raw_text }}</pre>
        </div>

        <div v-if="Object.keys(detailEntity.extracted_fields || {}).length">
          <div class="text-[rgba(245,245,220,0.40)] text-xs mb-1">Extracted fields</div>
          <div class="space-y-1">
            <div
              v-for="(val, key) in detailEntity.extracted_fields"
              :key="key"
              class="flex gap-2 text-[11px] items-start"
            >
              <span class="text-[#00d4ff] shrink-0">{{ key }}</span>
              <span
                class="text-[rgba(245,245,220,0.7)] font-mono break-all flex-1"
                :class="{ 'copy-blink': copiedKey === `e:${key}` }"
              >{{ String(val) }}</span>
              <button
                class="shrink-0 text-[rgba(245,245,220,0.3)] hover:text-[#00d4ff] transition-colors mt-0.5"
                :title="copiedKey === `e:${key}` ? 'Copied' : 'Copy value'"
                @click="copyValue(String(val), `e:${key}`)"
              >
                <component :is="copiedKey === `e:${key}` ? Check : Copy" :size="11" />
              </button>
            </div>
          </div>
        </div>
        </template>
      </div>
      <div v-else class="p-3 text-xs text-[rgba(245,245,220,0.4)] font-mono break-all">
        No record loaded for {{ detailId }}
      </div>
    </div>

    <!-- Edge data window (double-click an edge) -->
    <div
      v-if="edgeDetail"
      class="absolute bottom-2 right-2 w-64 max-w-[calc(100%-1rem)] max-h-[calc(100%-1rem)] overflow-y-auto rounded-lg bg-[#0f0f0f]/97 border shadow-2xl"
      :style="{ borderColor: edgeColor }"
    >
      <div
        class="flex items-center justify-between gap-2 px-3 py-2 border-b sticky top-0 bg-[#0f0f0f]"
        :style="{ borderColor: edgeColor + '33' }"
      >
        <div class="flex items-center gap-1.5 min-w-0">
          <Waypoints :size="13" class="shrink-0" :style="{ color: edgeColor }" />
          <span class="text-[10px] uppercase tracking-wide text-[rgba(245,245,220,0.4)] shrink-0">Edge</span>
          <span class="text-xs font-semibold text-[#f5f5dc] truncate">{{ edgeDetail.relation_type }}</span>
        </div>
        <button class="text-[rgba(245,245,220,0.45)] hover:text-[#f5f5dc] shrink-0" title="Close" @click="closeEdge">
          <X :size="14" />
        </button>
      </div>
      <div class="p-3">
        <dl class="space-y-1.5">
          <div v-for="row in edgeRows" :key="row.label" class="flex gap-2 text-xs items-start">
            <dt class="text-[rgba(245,245,220,0.40)] w-20 shrink-0">{{ row.label }}</dt>
            <dd
              class="text-[rgba(245,245,220,0.85)] font-mono break-all flex-1"
              :class="{ 'copy-blink': copiedKey === `g:${row.label}` }"
            >{{ row.value }}</dd>
            <button
              class="shrink-0 text-[rgba(245,245,220,0.3)] hover:text-[#00d4ff] transition-colors mt-0.5"
              :title="copiedKey === `g:${row.label}` ? 'Copied' : 'Copy value'"
              @click="copyValue(row.value, `g:${row.label}`)"
            >
              <component :is="copiedKey === `g:${row.label}` ? Check : Copy" :size="12" />
            </button>
          </div>
        </dl>
      </div>
    </div>
  </div>
</template>

<style scoped>
/* On copy, the value flashes the page's cyan and blinks a couple of times. */
.copy-blink {
  color: #00d4ff !important;
  animation: copy-blink 0.25s ease-in-out 1;
}
@keyframes copy-blink {
  0%,
  100% {
    opacity: 1;
  }
  50% {
    opacity: 0.2;
  }
}
</style>
