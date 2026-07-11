import { markRaw, defineAsyncComponent } from 'vue'
import type { RouteItem } from '@/types/menu'
import homeIcon from '@/assets/icons/sidebar-home.svg'
import devicesIcon from '@/assets/icons/sidebar-devices.svg'
import alarmIcon from '@/assets/icons/sidebar-alarm.svg'
import controlIcon from '@/assets/icons/sidebar-control.svg'
import statisticsIcon from '@/assets/icons/sidebar-statistics.svg'
import settingIcon from '@/assets/icons/sidebar-setting.svg'

// 工具：安全的异步组件（避免被代理）

export const dynamicRoutes: RouteItem[] = [
  {
    path: '/home',
    name: 'home',
    component: () => import('@/views/HomeView/index.vue'),
    meta: {
      isSubMenu: false,
      activeNav: '/home',
      icon: homeIcon,
      title: 'Home',
      roles: ['Admin', 'Viewer', 'Engineer'],
    },
  },
  {
    path: '/devices',
    name: 'devices',
    meta: {
      isSubMenu: true,
      activeNav: '/devices',
      icon: devicesIcon,
      title: 'Devices',
      roles: ['Admin', 'Viewer', 'Engineer'],
    },
    children: [
      {
        path: 'devicesPV',
        name: 'devicesPV',
        redirect: '/devices/devicesPV/overview',
        component: () => import('@/views/Devices/DevicesPV/index.vue'),
        meta: {
          title: 'PV',
          activeNav: '/devices/devicesPV',
          roles: ['Admin', 'Viewer', 'Engineer'],
        },
        children: [
          {
            path: 'overview',
            name: 'devicesPVOverview',
            component: () => import('@/views/Devices/DevicesPV/PVOverview.vue'),
            meta: {
              activeNav: '/devices/devicesPV',
              roles: ['Admin', 'Viewer', 'Engineer'],
            },
          },
          {
            path: 'monitoring',
            name: 'devicesPVMonitoring',
            component: () => import('@/views/Devices/DevicesPV/PVValueMonitoring.vue'),
            meta: {
              activeNav: '/devices/devicesPV',
              roles: ['Admin', 'Viewer', 'Engineer'],
            },
          },
        ],
      },
      {
        path: 'deviceBattery',
        name: 'deviceBattery',
        component: () => import('@/views/Devices/DeviceBattery/index.vue'),
        redirect: '/devices/deviceBattery/overview',
        meta: {
          title: 'Battery',
          activeNav: '/devices/deviceBattery',
          roles: ['Admin', 'Viewer', 'Engineer'],
        },
        children: [
          {
            path: 'overview',
            name: 'deviceBatteryOverview',
            component: () => import('@/views/Devices/DeviceBattery/BatteryOverview.vue'),
            meta: {
              activeNav: '/devices/deviceBattery',
              roles: ['Admin', 'Viewer', 'Engineer'],
            },
          },
          {
            path: 'value',
            name: 'deviceBatteryValue',
            component: () => import('@/views/Devices/DeviceBattery/BatteryValue.vue'),
            meta: {
              activeNav: '/devices/deviceBattery',
              roles: ['Admin', 'Viewer', 'Engineer'],
            },
          },
          {
            path: 'management',
            name: 'deviceBatteryManagement',
            component: () => import('@/views/Devices/DeviceBattery/BatteryManagement.vue'),
            meta: {
              activeNav: '/devices/deviceBattery',
              roles: ['Admin', 'Viewer', 'Engineer'],
            },
          },
        ],
      },
      {
        path: 'dieselGenerator',
        name: 'dieselGenerator',
        component: () => import('@/views/Devices/DieselGenerator/index.vue'),
        redirect: '/devices/dieselGenerator/overview',
        meta: {
          title: 'Diesel Generator',
          activeNav: '/devices/dieselGenerator',
          roles: ['Admin', 'Viewer', 'Engineer'],
        },
        children: [
          {
            path: 'overview',
            name: 'dieselGeneratorOverview',
            component: () => import('@/views/Devices/DieselGenerator/DieselOverview.vue'),
            meta: {
              activeNav: '/devices/dieselGenerator',
              roles: ['Admin', 'Viewer', 'Engineer'],
            },
          },
          {
            path: 'monitoring',
            name: 'dieselGeneratorMonitoring',
            component: () => import('@/views/Devices/DieselGenerator/DieselValueMonitoring.vue'),
            meta: {
              activeNav: '/devices/dieselGenerator',
              roles: ['Admin', 'Viewer', 'Engineer'],
            },
          },
        ],
      },
      // {
      //   path: 'devicePCS',
      //   name: 'devicePCS',
      //   component: () => import('@/views/DevicePCS/index.vue'),
      //   meta: {
      //     title: 'PCS',
      //     activeNav: '/devices/devicePCS',
      //     roles: ['Admin', 'Viewer', 'Engineer'],
      //   },
      // },
      {
        path: 'devicemeter1',
        name: 'devicemeter1',
        component: () => import('@/views/Devices/DeviceMeter1/index.vue'),
        meta: {
          title: 'Meter1',
          activeNav: '/devices/devicemeter1',
          roles: ['Admin', 'Viewer', 'Engineer'],
        },
      },
      {
        path: 'devicemeter2',
        name: 'devicemeter2',
        component: () => import('@/views/Devices/DeviceMeter2/index.vue'),
        meta: {
          title: 'Meter2',
          activeNav: '/devices/devicemeter2',
          roles: ['Admin', 'Viewer', 'Engineer'],
        },
      },
    ],
  },
  {
    path: '/alarm',
    name: 'alarm',
    meta: {
      isSubMenu: true,
      activeNav: '/alarm',
      icon: alarmIcon,
      title: 'Alarm',
      roles: ['Admin', 'Viewer', 'Engineer'],
    },
    children: [
      {
        path: 'alarmCurrentRecords',
        name: 'alarmCurrentRecords',
        component: () => import('@/views/Alarm/AlarmCurrentRecords/index.vue'),
        meta: {
          title: 'Current Records',
          activeNav: '/alarm/alarmCurrentRecords',
          roles: ['Admin', 'Viewer', 'Engineer'],
        },
      },
      {
        path: 'alarmHistoryRecords',
        name: 'alarmHistoryRecords',
        component: () => import('@/views/Alarm/AlarmHistoryRecords/index.vue'),
        meta: {
          title: 'History Records',
          activeNav: '/alarm/alarmHistoryRecords',
          roles: ['Admin', 'Viewer', 'Engineer'],
        },
      },
      {
        path: 'ruleManagement',
        name: 'ruleManagement',
        component: () => import('@/views/Alarm/RulesManagement/index.vue'),
        meta: {
          title: 'Rule Management',
          activeNav: '/alarm/ruleManagement',
          roles: ['Admin'],
        },
      },
    ],
  },
  {
    path: '/control',
    name: 'control',
    meta: {
      title: 'Control',
      isSubMenu: true,
      activeNav: '/control',
      icon: controlIcon,
      roles: ['Admin', 'Viewer', 'Engineer'],
    },
    children: [
      {
        path: 'controlRecord',
        name: 'controlRecord',
        component: () => import('@/views/Control/ControlRecord/index.vue'),
        meta: {
          title: 'Control Record',
          activeNav: '/control/controlRecord',
          roles: ['Admin', 'Viewer', 'Engineer'],
        },
      },
      {
        path: 'controlManagement',
        name: 'controlManagement',
        component: () => import('@/views/Control/ControlManagement/index.vue'),
        meta: {
          title: 'Control Management',
          activeNav: '/control/controlManagement',
          roles: ['Admin'],
        },
      },
    ],
  },
  {
    path: '/statistics',
    name: 'statistics',
    // component: () => import('@/views/Statistics/index.vue'),
    redirect: '/statistics/overview',
    meta: {
      isSubMenu: true,
      title: 'Statistics',
      activeNav: '/statistics',
      icon: statisticsIcon,
      roles: ['Admin', 'Viewer', 'Engineer'],
    },
    children: [
      {
        path: 'overview',
        name: 'statisticsOverview',
        component: () => import('@/views/Statistics/Overview.vue'),
        meta: {
          title: 'Overview',
          activeNav: '/statistics/overview',
          roles: ['Admin', 'Viewer', 'Engineer'],
        },
      },
      {
        path: 'curves',
        name: 'statisticsCurves',
        component: () => import('@/views/Statistics/Curves.vue'),
        meta: {
          title: 'Curves',
          activeNav: '/statistics/curves',
          roles: ['Admin', 'Viewer', 'Engineer'],
        },
      },
      {
        path: 'operationLog',
        name: 'statisticsOperationLog',
        component: () => import('@/views/Statistics/OperationLog.vue'),
        meta: {
          title: 'Operation Log',
          activeNav: '/statistics/operationLog',
          roles: ['Admin', 'Viewer', 'Engineer'],
        },
      },
      {
        path: 'runingLog',
        name: 'statisticsRuningLog',
        component: () => import('@/views/Statistics/RuningLog.vue'),
        meta: {
          title: 'Runing Log',
          activeNav: '/statistics/runingLog',
          roles: ['Admin', 'Viewer', 'Engineer'],
        },
      },
    ],
  },
  {
    path: '/setting',
    name: 'setting',
    meta: {
      isSubMenu: true,
      activeNav: '/setting',
      icon: settingIcon,
      title: 'Setting',
      roles: ['Admin'],
    },
    children: [
      {
        path: 'systemSetting',
        name: 'systemSetting',
        component: () => import('@/views/Setting/SystemSetting/index.vue'),
        meta: {
          title: 'System Setting',
          activeNav: '/setting/systemSetting',
          roles: ['Admin'],
        },
      },
      {
        path: 'userManagement',
        name: 'userManagement',
        component: () => import('@/views/Setting/UserManagement/index.vue'),
        meta: {
          title: 'User Management',
          activeNav: '/setting/userManagement',
          roles: ['Admin'],
        },
      },
      /*
      {
        path: 'configuration',
        name: 'configuration',
        component: () => import('@/views/Setting/Configuration/index.vue'),
        redirect: '/setting/configuration/channelConfiguration',
        meta: {
          title: 'Configuration',
          activeNav: '/setting/configuration',
          roles: ['Admin'],
        },
        children: [
          {
            path: 'channelConfiguration',
            name: 'channelConfiguration',
            component: () => import('@/views/Setting/Configuration/ChannelConfiguration/index.vue'),
            meta: {
              title: 'Channel Configuration',
              activeNav: '/setting/configuration',
              roles: ['Admin'],
            },
          },
          {
            path: 'modelConfiguration',
            name: 'modelConfiguration',
            component: () => import('@/views/Setting/Configuration/DeviceConfiguration/index.vue'),
            meta: {
              title: 'Model Configuration',
              activeNav: '/setting/configuration',
              roles: ['Admin'],
            },
          },
          {
            path: 'ruleConfiguration',
            name: 'ruleConfiguration',
            component: () => import('@/views/Setting/Configuration/RuleConfiguration/index.vue'),

            meta: {
              title: 'Rule Configuration',
              activeNav: '/setting/configuration',
              roles: ['Admin'],
            },
          },
          {
            path: 'ruleChainEditor/:id',
            name: 'ruleChainEditor',
            component: () =>
              import('@/views/Setting/Configuration/RuleConfiguration/RuleChainEditorView.vue'),
            meta: {
              title: 'Rule Chain Editor',
              activeNav: '/setting/configuration',
              roles: ['Admin'],
            },
          },
        ],
      },
      */
    ],
  },
]
