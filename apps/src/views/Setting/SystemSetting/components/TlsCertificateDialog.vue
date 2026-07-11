<template>
  <FormDialog
    ref="formDialogRef"
    title="TLS Certificate Config"
    width="7.2rem"
    @close="handleClose"
  >
    <template #dialog-body>
      <div class="aether-class tls-certificate-dialog">
        <div class="cert-dir" v-if="certDir">Certificate Directory: {{ certDir }}</div>

        <div class="cert-table">
          <div v-for="item in certItems" :key="item.type" class="cert-row">
            <div class="cert-row__meta">
              <div class="cert-row__title">{{ item.label }}</div>
              <div class="cert-row__desc">
                Default Filename: <span>{{ item.defaultName }}</span>
              </div>
              <div class="cert-row__status">
                Status:
                <span :class="certState[item.type].exists ? 'is-success' : 'is-empty'">
                  {{ certState[item.type].exists ? 'Configured' : 'Not Configured' }}
                </span>
              </div>
            </div>

            <div class="cert-row__actions">
              <el-upload
                :auto-upload="false"
                :show-file-list="false"
                :on-change="handleUploadChange(item.type)"
                :accept="item.ext"
              >
                <el-button type="primary" :loading="uploadingType === item.type">
                  Upload({{ item.ext }})
                </el-button>
              </el-upload>
              <el-button
                type="danger"
                plain
                :disabled="!certState[item.type].exists"
                :loading="deletingType === item.type"
                @click="handleDelete(item.type)"
              >
                Delete
              </el-button>
            </div>
          </div>
        </div>
      </div>
    </template>

    <template #dialog-footer>
      <el-button @click="close">Close</el-button>
    </template>
  </FormDialog>
</template>

<script setup lang="ts">
import type { UploadFile } from 'element-plus'
import {
  deleteCertificate,
  getCertificateInfo,
  uploadCertificate,
  type CertificateType,
} from '@/api/System'

interface CertState {
  exists: boolean
}

const extFromDefaultName = (fileName: string) => {
  const i = fileName.lastIndexOf('.')
  return i >= 0 ? fileName.slice(i).toLowerCase() : ''
}

const certItemsRaw: Array<{ type: CertificateType; label: string; defaultName: string }> = [
  { type: 'ca_cert', label: 'CA Certificate', defaultName: 'AmazonRootCA1.pem' },
  { type: 'client_cert', label: 'Client Certificate', defaultName: 'certificate.pem.crt' },
  { type: 'client_key', label: 'Client Key', defaultName: 'private.pem.key' },
]
const certItems = certItemsRaw.map((row) => ({
  ...row,
  ext: extFromDefaultName(row.defaultName),
}))

const formDialogRef = ref<{ dialogVisible: boolean } | null>(null)
const uploadingType = ref<CertificateType | ''>('')
const deletingType = ref<CertificateType | ''>('')
const certDir = ref('')
const certState = ref<Record<CertificateType, CertState>>({
  ca_cert: { exists: false },
  client_cert: { exists: false },
  client_key: { exists: false },
})

const normalizeInfo = (data: any) => {
  certDir.value = String(data?.cert_dir || '')
  const next: Record<CertificateType, CertState> = {
    ca_cert: { exists: false },
    client_cert: { exists: false },
    client_key: { exists: false },
  }

  const files = Array.isArray(data?.files) ? data.files : []
  certItems.forEach((item) => {
    const fileInfo = files.find(
      (f: any) => String(f?.file || '').toLowerCase() === item.defaultName.toLowerCase(),
    )
    if (!fileInfo) return
    next[item.type].exists = Boolean(fileInfo.exists)
  })

  certState.value = next
}

const fetchInfo = async () => {
  const res = await getCertificateInfo()
  if (res.success) {
    normalizeInfo(res.data)
  } else {
    ElMessage.error(res.message || 'Failed to fetch certificate info')
  }
}

const open = async () => {
  await fetchInfo()
  if (formDialogRef.value) {
    formDialogRef.value.dialogVisible = true
  }
}

const close = () => {
  if (formDialogRef.value) {
    formDialogRef.value.dialogVisible = false
  }
}

const handleClose = () => {
  // no-op
}

const validateFile = (file: File | undefined, expectedExt: string) => {
  if (!file) return 'Please select a certificate file'
  const lower = file.name.toLowerCase()
  if (!expectedExt || !lower.endsWith(expectedExt)) {
    return `File suffix must be ${expectedExt}`
  }
  return ''
}

const handleUpload = async (certType: CertificateType, uploadFile: UploadFile) => {
  const rawFile = uploadFile.raw
  const item = certItems.find((c) => c.type === certType)
  const expectedExt = item?.ext || ''
  const message = validateFile(rawFile, expectedExt)
  if (message) {
    ElMessage.warning(message)
    return
  }
  try {
    uploadingType.value = certType
    const res = await uploadCertificate(certType, rawFile as File)
    if (res.success) {
      ElMessage.success(res.message || 'Certificate uploaded successfully')
      await fetchInfo()
      return
    }
    ElMessage.error(res.message || 'Failed to upload certificate')
  } finally {
    uploadingType.value = ''
  }
}

const handleUploadChange = (certType: CertificateType) => {
  return (uploadFile: UploadFile) => handleUpload(certType, uploadFile)
}

const handleDelete = async (certType: CertificateType) => {
  try {
    deletingType.value = certType
    const res = await deleteCertificate(certType)
    if (res.success) {
      ElMessage.success(res.message || 'Certificate deleted successfully')
      await fetchInfo()
      return
    }
    ElMessage.error(res.message || 'Failed to delete certificate')
  } finally {
    deletingType.value = ''
  }
}

defineExpose({ open, close })
</script>

<style scoped lang="scss">
.aether-class {
  .tls-certificate-dialog {
    display: flex;
    flex-direction: column;
    gap: 0.16rem;
    padding-bottom: 0.2rem;
  }

  .cert-table {
    display: flex;
    flex-direction: column;
    gap: 0.12rem;
    max-height: 4rem;
    overflow-y: auto;
  }

  .cert-dir {
    font-size: 0.13rem;
    color: rgba(245, 247, 255, 0.9);
    word-break: break-all;
  }

  .cert-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.2rem;
    padding: 0.14rem;
    border: 1px solid rgba(255, 255, 255, 0.15);
    border-radius: 0.08rem;
    background: rgba(44, 66, 106, 0.1);
  }

  .cert-row__title {
    font-size: 0.16rem;
    font-weight: 700;
    color: #fff;
  }

  .cert-row__desc,
  .cert-row__status {
    margin-top: 0.04rem;
    font-size: 0.12rem;
    color: rgba(245, 247, 255, 0.9);
    line-height: 1.5;
    word-break: break-all;
  }

  .cert-row__actions {
    display: flex;
    gap: 0.1rem;
  }

  .is-success {
    color: #67c23a;
    font-weight: 700;
  }

  .is-empty {
    color: #f56c6c;
    font-weight: 700;
  }
}
</style>
