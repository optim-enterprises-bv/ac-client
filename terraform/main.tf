# OptimACS Deployment via SSH to Proxmox
# Creates VM using SSH/qm commands instead of Proxmox API

terraform {
  required_version = ">= 1.0"
  required_providers {
    null = {
      source  = "hashicorp/null"
      version = "~> 3.2"
    }
  }
}

# Random password for admin user
resource "random_password" "admin_password" {
  length           = 16
  special          = true
  override_special = "!@#$%^&*()_+-="
}

# Variables
variable "proxmox_host" {
  description = "Proxmox host IP/hostname"
  type        = string
  default     = "192.168.10.245"
}

variable "proxmox_ssh_user" {
  description = "SSH user for Proxmox host"
  type        = string
  default     = "root"
}

variable "proxmox_ssh_key" {
  description = "Path to SSH private key for Proxmox"
  type        = string
  default     = "~/.ssh/id_rsa"
}

variable "proxmox_template" {
  description = "VM template ID to clone"
  type        = string
  default     = "9000"
}

variable "proxmox_storage" {
  description = "Storage pool"
  type        = string
  default     = "local-lvm"
}

variable "vm_name" {
  description = "Name for the new VM"
  type        = string
  default     = "optimacs-server"
}

variable "vm_id" {
  description = "VM ID (leave empty for auto)"
  type        = string
  default     = ""
}

variable "vm_cores" {
  description = "CPU cores"
  type        = number
  default     = 4
}

variable "vm_memory" {
  description = "RAM in MB"
  type        = number
  default     = 8192
}

variable "vm_disk_size" {
  description = "Disk size"
  type        = string
  default     = "50G"
}

variable "vm_bridge" {
  description = "Network bridge"
  type        = string
  default     = "vmbr0"
}

variable "ssh_user" {
  description = "SSH user for the VM"
  type        = string
  default     = "debian"
}

variable "ssh_public_key" {
  description = "Path to SSH public key for VM"
  type        = string
  default     = "~/.ssh/id_rsa.pub"
}

variable "env_file_path" {
  description = "Path to .env file"
  type        = string
  default     = "../.env"
}

variable "stepca_password" {
  description = "step-ca password"
  type        = string
  sensitive   = true
  default     = "4nn1343v3r"
}

variable "stepca_provisioner_name" {
  description = "step-ca JWK provisioner name (used as JWT issuer)"
  type        = string
  default     = "acserver@optimacs.local"
}

variable "stepca_provisioner_key_path" {
  description = "Path to step-ca provisioner private key file (PKCS#8 PEM format)"
  type        = string
  default     = ""
}

variable "admin_username" {
  description = "Admin username"
  type        = string
  default     = "admin"
}

variable "admin_password" {
  description = "Admin password"
  type        = string
  sensitive   = true
  default     = ""
}

variable "vm_ip" {
  description = "Static IP for VM (e.g., 192.168.10.5)"
  type        = string
  default     = "192.168.10.5"
}

variable "vm_gateway" {
  description = "Gateway for VM"
  type        = string
  default     = "192.168.10.1"
}

# Local values
locals {
  vm_id       = var.vm_id != "" ? var.vm_id : "${random_integer.vm_id.result}"
  template_id = "9000"
}

# Generate random VM ID
resource "random_integer" "vm_id" {
  min = 100
  max = 999
}

# Create Debian 12 template on Proxmox
resource "null_resource" "create_template" {
  triggers = {
    template_id = local.template_id
  }

  connection {
    type        = "ssh"
    user        = var.proxmox_ssh_user
    private_key = file(var.proxmox_ssh_key)
    host        = var.proxmox_host
  }

  provisioner "remote-exec" {
    inline = [
      "echo 'Checking if template ${local.template_id} exists...'",
      "if qm status ${local.template_id} >/dev/null 2>&1; then",
      "  echo 'Template ${local.template_id} already exists, using existing template'",
      "  exit 0",
      "fi",
      "echo 'Creating Debian 12 template...'",
      "cd /tmp",
      "echo 'Downloading Debian 12 cloud image...'",
      "if [ ! -f debian-12-generic-amd64.qcow2 ]; then",
      "  wget -q --show-progress https://cloud.debian.org/images/cloud/bookworm/latest/debian-12-generic-amd64.qcow2 2>/dev/null || curl -L -o debian-12-generic-amd64.qcow2 https://cloud.debian.org/images/cloud/bookworm/latest/debian-12-generic-amd64.qcow2",
      "fi",
      "echo 'Creating VM ${local.template_id} for template...'",
      "qm create ${local.template_id} --name debian-12-cloudinit --memory 2048 --cores 2 --cpu host --net0 virtio,bridge=vmbr0",
      "echo 'Importing disk...'",
      "qm importdisk ${local.template_id} debian-12-generic-amd64.qcow2 local-lvm",
      "qm set ${local.template_id} --scsihw virtio-scsi-pci --scsi0 local-lvm:vm-${local.template_id}-disk-0",
      "echo 'Adding cloud-init drive...'",
      "qm set ${local.template_id} --ide2 local-lvm:cloudinit",
      "qm set ${local.template_id} --boot order=scsi0",
      "qm set ${local.template_id} --serial0 socket --vga serial0",
      "echo 'Configuring cloud-init...'",
      "qm set ${local.template_id} --ciuser debian",
      "qm set ${local.template_id} --cipassword debian",
      "if [ -f '/root/.ssh/authorized_keys' ]; then",
      "  qm set ${local.template_id} --sshkeys /root/.ssh/authorized_keys",
      "fi",
      "echo 'Converting to template...'",
      "qm template ${local.template_id}",
      "echo 'Debian 12 template ${local.template_id} created successfully!'"
    ]
  }
}

# Create VM on Proxmox via SSH
resource "null_resource" "create_vm" {
  depends_on = [null_resource.create_template]
  triggers = {
    vm_name = var.vm_name
    vm_id   = local.vm_id
  }

  connection {
    type        = "ssh"
    user        = var.proxmox_ssh_user
    private_key = file(var.proxmox_ssh_key)
    host        = var.proxmox_host
  }

  provisioner "remote-exec" {
    inline = [
      "echo 'Checking if VM ${local.vm_id} exists...'",
      "if qm status ${local.vm_id} >/dev/null 2>&1; then",
      "  echo 'VM ${local.vm_id} already exists, destroying...'",
      "  qm stop ${local.vm_id} || true",
      "  sleep 2",
      "  qm destroy ${local.vm_id} || true",
      "fi",
      "echo 'Cloning template ${local.template_id} to VM ${local.vm_id}...'",
      "qm clone ${local.template_id} ${local.vm_id} --name ${var.vm_name} --full 1",
      "echo 'Configuring VM with static IP ${var.vm_ip}...'",
      "qm set ${local.vm_id} --cores ${var.vm_cores}",
      "qm set ${local.vm_id} --memory ${var.vm_memory}",
      "qm set ${local.vm_id} --cpu host",
      "qm resize ${local.vm_id} scsi0 ${var.vm_disk_size}",
      "qm set ${local.vm_id} --net0 virtio,bridge=vmbr0",
      "echo 'Setting static IP ${var.vm_ip} with gateway ${var.vm_gateway}...'",
      "qm set ${local.vm_id} --ipconfig0 ip=${var.vm_ip}/24,gw=${var.vm_gateway}",
      "qm set ${local.vm_id} --cipassword rootpassword123",
      "if [ -f '${var.ssh_public_key}' ]; then",
      "  qm set ${local.vm_id} --sshkeys '${var.ssh_public_key}'",
      "fi",
      "echo 'Starting VM ${local.vm_id}...'",
      "qm start ${local.vm_id}",
      "echo 'VM configured with static IP: ${var.vm_ip}'",
      "echo '${var.vm_ip}' > /tmp/optimacs_vm_ip_${local.vm_id}.txt",
      "sleep 60"
    ]
  }
}

# Deploy OptimACS to the VM (using static IP)
resource "null_resource" "deploy_optimacs" {
  depends_on = [null_resource.create_vm]

  connection {
    type        = "ssh"
    user        = var.proxmox_ssh_user
    private_key = file(var.proxmox_ssh_key)
    host        = var.proxmox_host
  }

  provisioner "remote-exec" {
    inline = [
      "echo 'Using static IP: ${var.vm_ip}'",
      "echo 'Waiting for VM SSH to be ready...'",
      "for i in $(seq 1 60); do",
      "  if ssh -o StrictHostKeyChecking=no -o ConnectTimeout=5 -o PasswordAuthentication=no ${var.ssh_user}@${var.vm_ip} 'echo ok' 2>/dev/null; then",
      "    echo 'VM is ready'",
      "    break",
      "  fi",
      "  echo 'Waiting for VM SSH...'",
      "  sleep 5",
      "done",
      "echo 'VM should be ready at ${var.vm_ip}'"
    ]
  }
}

# Copy files and start services directly on VM
resource "null_resource" "start_optimacs" {
  depends_on = [null_resource.deploy_optimacs]

  provisioner "local-exec" {
    command = <<-EOT
      echo "Creating app directory on VM..."
      ssh -o StrictHostKeyChecking=no ${var.ssh_user}@${var.vm_ip} 'sudo mkdir -p /opt/optimacs && sudo chown ${var.ssh_user}:${var.ssh_user} /opt/optimacs'
      
      echo "Copying OptimACS files..."
      scp -o StrictHostKeyChecking=no -r ${path.module}/../* ${var.ssh_user}@${var.vm_ip}:/opt/optimacs/
      scp -o StrictHostKeyChecking=no ${var.env_file_path} ${var.ssh_user}@${var.vm_ip}:/opt/optimacs/.env
      
      echo "Creating step-ca provisioner key directory..."
      ssh -o StrictHostKeyChecking=no ${var.ssh_user}@${var.vm_ip} 'sudo mkdir -p /opt/optimacs/docker/stepca && sudo chmod 700 /opt/optimacs/docker/stepca'
      
      %{if var.stepca_provisioner_key_path != ""}
      echo "Copying step-ca provisioner key..."
      scp -o StrictHostKeyChecking=no ${var.stepca_provisioner_key_path} ${var.ssh_user}@${var.vm_ip}:/opt/optimacs/docker/stepca/provisioner.key
      ssh -o StrictHostKeyChecking=no ${var.ssh_user}@${var.vm_ip} 'sudo chmod 600 /opt/optimacs/docker/stepca/provisioner.key'
      %{endif}
      
      echo "Updating .env with step-ca configuration..."
      ssh -o StrictHostKeyChecking=no ${var.ssh_user}@${var.vm_ip} '
        if ! grep -q "STEPCA_PROVISIONER_NAME" /opt/optimacs/.env; then
          echo "" >> /opt/optimacs/.env
          echo "# Step-CA JWK Provisioner Configuration" >> /opt/optimacs/.env
          echo "STEPCA_PROVISIONER_NAME=${var.stepca_provisioner_name}" >> /opt/optimacs/.env
        fi
      '
      
      echo "Installing Docker..."
      ssh -o StrictHostKeyChecking=no ${var.ssh_user}@${var.vm_ip} 'curl -fsSL https://get.docker.com | sh'
      ssh -o StrictHostKeyChecking=no ${var.ssh_user}@${var.vm_ip} 'sudo usermod -aG docker ${var.ssh_user}'
      
      echo "Starting OptimACS services..."
      ssh -o StrictHostKeyChecking=no ${var.ssh_user}@${var.vm_ip} 'cd /opt/optimacs && sudo docker compose up -d'
      
      echo "Waiting for MySQL to be ready..."
      sleep 30
      
      echo "Initializing Databunker..."
      ssh -o StrictHostKeyChecking=no ${var.ssh_user}@${var.vm_ip} 'cd /opt/optimacs && sudo docker compose exec -T databunker /databunker -init || echo "Databunker init may have already been done"'
      
      echo "Waiting for all services to start..."
      sleep 60
      
      echo "Checking service status..."
      ssh -o StrictHostKeyChecking=no ${var.ssh_user}@${var.vm_ip} 'cd /opt/optimacs && sudo docker compose ps'
      
      echo ""
      echo "========================================"
      echo "DEPLOYMENT COMPLETE"
      echo "========================================"
      echo "VM IP: ${var.vm_ip}"
      echo "Management UI: http://${var.vm_ip}:8080"
      echo "EMQX Dashboard: http://${var.vm_ip}:18083"
      echo "Grafana: http://${var.vm_ip}:3000"
    EOT
  }
}

# Create admin user in the UI
resource "null_resource" "create_admin_user" {
  depends_on = [null_resource.start_optimacs]

  provisioner "local-exec" {
    command = <<-EOT
      echo "Creating admin user in OptimACS UI..."
      
      # Wait for UI to be healthy
      for i in $(seq 1 30); do
        if ssh -o StrictHostKeyChecking=no ${var.ssh_user}@${var.vm_ip} 'cd /opt/optimacs && sudo docker compose ps optimacs-ui | grep -q healthy'; then
          echo "UI is healthy, creating admin user..."
          break
        fi
        echo "Waiting for UI to be healthy... (attempt $i/30)"
        sleep 5
      done
      
      # Create admin user using create_admin.py inside the UI container
      ssh -o StrictHostKeyChecking=no ${var.ssh_user}@${var.vm_ip} 'cd /opt/optimacs && sudo docker compose exec -T optimacs-ui python create_admin.py --username ${var.admin_username} --password "${random_password.admin_password.result}" --role full_admin'
      
      echo ""
      echo "========================================"
      echo "ADMIN USER CREATED"
      echo "========================================"
      echo "Username: ${var.admin_username}"
      echo "Password: ${random_password.admin_password.result}"
      echo "========================================"
    EOT
  }
}

# Outputs
output "vm_id" {
  description = "Created VM ID"
  value       = local.vm_id
}

output "vm_name" {
  description = "VM name"
  value       = var.vm_name
}

output "vm_ip" {
  description = "VM static IP address"
  value       = var.vm_ip
}

output "deployment_status" {
  description = "Deployment instructions"
  value       = "VM created with static IP ${var.vm_ip}. Access services at: http://${var.vm_ip}:8080 (UI), http://${var.vm_ip}:18083 (EMQX), http://${var.vm_ip}:3000 (Grafana)"
}

output "admin_username" {
  description = "Admin username for the UI"
  value       = var.admin_username
}

output "admin_password" {
  description = "Admin password for the UI (sensitive)"
  value       = random_password.admin_password.result
  sensitive   = true
}
