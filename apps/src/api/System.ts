import { Request } from '@/utils/request'

export const getMqttConfig = () => {
  return Request.get('/netApi/mqtt/config')
}

export const updateMqttConfig = (params: any) => {
  return Request.post('/netApi/mqtt/config', params)
}

export const disconnectMqtt = () => {
  return Request.post('/netApi/mqtt/disconnect')
}

export const reconnectMqtt = () => {
  return Request.post('/netApi/mqtt/reconnect')
}
export const getMqttStatus = () => {
  return Request.get('/netApi/mqtt/status')
}

export type CertificateType = 'ca_cert' | 'client_cert' | 'client_key'

export const getCertificateInfo = () => {
  return Request.get('/netApi/certificate/info')
}

export const uploadCertificate = (certType: CertificateType, file: File) => {
  return Request.upload('/netApi/certificate/upload', file, { cert_type: certType })
}

export const deleteCertificate = (certType: CertificateType) => {
  return Request.delete(`/netApi/certificate/${certType}`)
}
