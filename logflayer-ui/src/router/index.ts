import { createRouter, createWebHistory } from 'vue-router'

const router = createRouter({
  history: createWebHistory(import.meta.env.BASE_URL),
  routes: [
    {
      path: '/',
      name: 'dashboard',
      component: () => import('../views/DashboardView.vue'),
      meta: { title: 'Dashboard' },
    },
    {
      path: '/targets',
      name: 'targets',
      component: () => import('../views/TargetsView.vue'),
      meta: { title: 'Targets' },
    },
    {
      path: '/logs',
      name: 'logs',
      component: () => import('../views/LogsView.vue'),
      meta: { title: 'Live Logs' },
    },
    {
      path: '/tracking',
      name: 'tracking',
      component: () => import('../views/TrackerView.vue'),
      meta: { title: 'Logging Tracker' },
    },
    {
      path: '/samples',
      name: 'samples',
      component: () => import('../views/SamplesView.vue'),
      meta: { title: 'Samples Explorer' },
    },
    {
      path: '/classifications',
      name: 'classifications',
      component: () => import('../views/ClassificationsView.vue'),
      meta: { title: 'Classifications' },
    },
    {
      path: '/deletions',
      name: 'deletions',
      component: () => import('../views/DeletionsView.vue'),
      meta: { title: 'Sample Deletions' },
    },
    {
      path: '/admin',
      name: 'admin',
      component: () => import('../views/AdminView.vue'),
      meta: { title: 'Admin Settings' },
    },
    // ── UpsideGate views ──────────────────────────────────────────────────────
    {
      path: '/upsidegate/entities',
      name: 'ug-entities',
      component: () => import('../views/upsidegate/EntitiesView.vue'),
      meta: { title: 'Entity Browser' },
    },
    {
      path: '/upsidegate/relations',
      name: 'ug-relations',
      component: () => import('../views/upsidegate/RelationsView.vue'),
      meta: { title: 'Relation Graph' },
    },
    {
      path: '/upsidegate/prov',
      name: 'ug-prov',
      component: () => import('../views/upsidegate/ProvView.vue'),
      meta: { title: 'PROV-O Triples' },
    },
    {
      path: '/upsidegate/spans',
      name: 'ug-spans',
      component: () => import('../views/upsidegate/SpansView.vue'),
      meta: { title: 'OTel Spans' },
    },
    // Detached, chrome-less graph window (opened via the graph's detach button).
    {
      path: '/graph',
      name: 'detached-graph',
      component: () => import('../views/upsidegate/DetachedGraphView.vue'),
      meta: { title: 'Relation Graph', bare: true },
    },
  ],
})

router.beforeEach((to, _from, next) => {
  document.title = `${to.meta.title || 'Logflayer'} | Logflayer`
  next()
})

export default router
