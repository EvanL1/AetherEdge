<template>
  <div class="aether-class header">
    <div class="header__left">
      <div class="header__left-title">Norton Creek Solar Energy Center</div>
      <div class="header__left-status">
        <div class="header__left-statusIcon">
          <div class="header__left-statusIconCircle"></div>
        </div>
        <div class="header__left-statusText">Online</div>
      </div>
    </div>

    <div class="header__right">
      <div class="header__right-weather">
        <img :src="sunIcon" alt="sunny weather" class="header__right-weatherIcon" />
        <div class="header__right-weatherStatus">Wind</div>
        <div class="header__right-weatherValue">67 ℉ / 79 ℉</div>
      </div>

      <div class="header__right-avatar">
        <el-dropdown trigger="click" @command="handleUserCommand">
          <div class="header__user">
            <div class="header__user-avatar">
              <div class="header__user-avatar-initials">
                {{ getAvatarName(userStore.userInfo?.username || 'Admin') }}
              </div>
            </div>
            <span class="header__user-name">{{ userStore.userInfo?.username || '' }}</span>
            <img :src="arrowDownIcon" alt="toggle user menu" class="header__user-arrow" />
          </div>

          <template #dropdown>
            <el-dropdown-menu>
              <el-dropdown-item command="logout">
                <div class="header__user-Item">
                  <img :src="logoutIcon" alt="logout" class="header__user-logoutIcon" />
                  Logout
                </div>
              </el-dropdown-item>
            </el-dropdown-menu>
          </template>
        </el-dropdown>
      </div>

      <div class="header__right-notice">
        <el-button link class="header__right-noticeBtn" @click="toggleNotifications">
          <el-badge
            :value="globalStore.alarmNum"
            :hidden="globalStore.alarmNum === 0"
            class="header__right-noticeBadge"
          >
            <img :src="noticeIcon" alt="notifications" class="header__right-noticeIcon" />
          </el-badge>
        </el-button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { useRouter } from 'vue-router'

import arrowDownIcon from '@/assets/icons/arrowDownIcon.svg'
import logoutIcon from '@/assets/icons/user-logout.svg'
import noticeIcon from '@/assets/icons/notice.svg'
import sunIcon from '@/assets/icons/sunny.svg'
import { useGlobalStore } from '@/stores/global'
import { useUserStore } from '@/stores/user'

const router = useRouter()
const userStore = useUserStore()
const globalStore = useGlobalStore()

const toggleNotifications = () => {
  router.push({ name: 'alarmCurrentRecords' })
}

const handleUserCommand = async (command: string) => {
  if (command !== 'logout') return

  await userStore.logout()
  router.push('/login')
}

const getAvatarName = (name: string) => {
  const parts = name.trim().split(/\s+/).filter(Boolean)

  if (parts.length <= 1) {
    return (parts[0] || 'A').charAt(0).toUpperCase()
  }

  return `${parts[0].charAt(0)}${parts[1].charAt(0)}`.toUpperCase()
}
</script>

<style lang="scss" scoped>
.aether-class.header {
  position: relative;
  z-index: 99;
  display: flex;
  align-items: center;
  justify-content: space-between;
  height: 0.85rem;
  padding: 0.2rem;
  background: rgba(84, 98, 140, 0.3);
  border-bottom: 0.01rem solid rgba(148, 166, 197, 0.3);

  .header__left {
    display: flex;
    align-items: center;
    gap: 0.1rem;

    .header__left-title {
      margin-right: 0.1rem;
      font-family: Montserrat;
      font-size: 0.3rem;
      font-style: normal;
      font-weight: 600;
      line-height: 150%;
      letter-spacing: 0;
      color: #fff;
    }

    .header__left-status {
      display: flex;
      align-items: center;
      width: 1rem;
      height: 0.3rem;
      padding: 0.07rem 0 0.07rem 0.1rem;
      background: rgba(84, 98, 140, 0.5);
      border: 0.01rem solid transparent;
      border-radius: 0.15rem;
      backdrop-filter: blur(0.1rem);

      .header__left-statusIcon {
        display: flex;
        align-items: center;
        justify-content: center;
        width: 0.16rem;
        height: 0.16rem;
        margin-right: 0.06rem;
        background-color: rgba(106, 193, 97, 0.2);
        border-radius: 50%;

        .header__left-statusIconCircle {
          width: 0.1rem;
          height: 0.1rem;
          background-color: rgb(106, 193, 97);
          border-radius: 50%;
        }
      }

      .header__left-statusText {
        font-family: Arimo;
        font-size: 0.16rem;
        font-style: normal;
        font-weight: 700;
        line-height: 100%;
        letter-spacing: 0;
        vertical-align: middle;
        color: #fff;
      }
    }
  }

  .header__right {
    display: flex;
    flex: 1;
    align-items: center;
    justify-content: flex-end;

    .header__right-weather {
      display: flex;
      align-items: center;
      gap: 0.1rem;
      margin-right: 1.26rem;
      font-size: 0.2rem;
      letter-spacing: 0;

      .header__right-weatherIcon {
        width: 0.4rem;
        height: 0.36rem;
        object-fit: contain;
      }

      .header__right-weatherStatus {
        font-style: normal;
        font-weight: 500;
      }

      .header__right-weatherValue {
        font-style: normal;
        font-weight: 700;
      }
    }

    .header__right-avatar {
      margin-right: 0.3rem;
      cursor: pointer;
    }

    .header__right-notice {
      .header__right-noticeBtn {
        display: flex;
        align-items: center;
        justify-content: center;
        transition: all 0.3s ease;

        :deep(.el-badge__content) {
          width: 0.16rem;
          height: 0.16rem;
          font-family: Arimo;
          font-size: 0.14rem;
          font-weight: 400;
          background-color: rgb(218, 45, 44);
          border: none;
          border-radius: 50%;
        }

        .header__right-noticeIcon {
          width: 0.2rem;
          height: 0.2rem;
          object-fit: contain;
        }
      }
    }
  }
}

.header__user {
  display: flex;
  align-items: center;
  gap: 0.08rem;
  padding: 0.04rem 0.08rem;
  border-radius: 0.08rem;
  transition: all 0.3s ease;
}

.header__user-avatar {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 0.4rem;
  height: 0.4rem;
  background-color: rgba(29, 134, 255, 0.2);
  border-radius: 50%;

  .header__user-avatar-initials {
    font-size: 0.19rem;
    font-weight: 700;
    line-height: 100%;
    letter-spacing: 0;
    color: rgb(29, 134, 255);
  }
}

.header__user-name {
  font-family: Arimo;
  font-size: 0.18rem;
  font-style: normal;
  font-weight: 500;
  line-height: 140%;
  letter-spacing: 0;
  color: #fff;
}

.header__user-arrow {
  width: 0.1rem;
  height: 0.08rem;
  font-size: 0.12rem;
  color: #909399;
}

.header__right-noticeBadge {
  display: flex;
  align-items: center;
  justify-content: center;
}

:deep(.header__right-noticeBadge .el-badge__content.is-fixed) {
  top: 0.04rem;
  right: 0.1rem;
  padding: 0;
}

.header__user-Item {
  display: flex;
  align-items: center;
  width: 100%;
  padding: 0.1rem;
  font-size: 0.14rem;
  font-weight: 500;
  line-height: 100%;
  letter-spacing: 0;
  color: #fff;

  .header__user-logoutIcon {
    width: 0.2rem;
    height: 0.2rem;
    margin-right: 0.1rem;
    object-fit: contain;
  }
}
</style>
