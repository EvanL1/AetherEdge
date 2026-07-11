# 构建阶段
FROM node:22-alpine AS builder

# 设置工作目录
WORKDIR /app

# 启用 corepack 并准备 pnpm（自动使用 package.json 中指定的版本）
RUN corepack enable && corepack prepare pnpm@latest --activate

# 复制 package.json 和 pnpm-lock.yaml
COPY package.json pnpm-lock.yaml ./

# 安装依赖
RUN pnpm install --frozen-lockfile

# 复制所有源代码
COPY . .

# 构建项目
RUN pnpm run build

# 生产阶段
FROM nginx:alpine

# 复制自定义 nginx 配置
COPY nginx.conf /etc/nginx/conf.d/default.conf

# 从构建阶段复制构建产物
COPY --from=builder /app/dist /usr/share/nginx/html

# 暴露端口 (nginx.conf 中配置为 8080)
EXPOSE 8080

# 启动 nginx
CMD ["nginx", "-g", "daemon off;"]
