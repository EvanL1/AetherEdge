export interface EnergyCard {
  id: number
  title?: string
  icon: string
  value?: unknown
  unit?: string
}

/** 首页点位 ID 与展示区域对应关系（参考 home-configuration-points-export.md） */
export const HOMEPAGE_POINT_IDS = {
  /** Energy Card: 1-4 */
  energyCard: [1, 2, 3, 4] as const,
  /** Station information: 5-7 */
  stationInfo: [5, 6, 7] as const,
  /** Device information - PV: 8(P), 9(U); Diesel: 10(P), 11(U); ESS: 12(P), 19(SOC) */
  deviceInfo: [
    [8, 9],
    [10, 11],
    [12, 13],
  ] as const,
  /** 拓朴图: pv.P=5, load.P=14, diesel.p=6, diesel.oil=17, ess.p=12, ess.soc=19 */
  topology: {
    pv: { P: 14 },
    load: { P: 15 },
    diesel: { p: 16, oil: 17 },
    ess: { p: 18, soc: 19 },
  } as const,
} as const
