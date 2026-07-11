import { Request } from '@/utils/request'

export const userManagementApi = {
  getUserList(params: any): Promise<any> {
    return Request.get('/api/v1/user/list', params)
  },
  addUser(params: any): Promise<any> {
    return Request.post('/api/v1/user/add', params)
  },
  updateUser(params: any): Promise<any> {
    return Request.put('/api/v1/user/update', params)
  },
  deleteUser(params: any): Promise<any> {
    return Request.delete('/api/v1/user/delete', { params })
  },
}
