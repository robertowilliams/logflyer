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
  ],
})

router.beforeEach((to, _from, next) => {
  document.title = `${to.meta.title || 'Logflayer'} | Logflayer`
  next()
})

export default router
