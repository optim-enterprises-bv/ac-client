# OptimACS Terraform Deployment for Proxmox

This directory contains Terraform configuration to deploy OptimACS on Proxmox via SSH.

## Features

- **Provision a new Proxmox VM** from a cloud-init template, or
- **Deploy to an existing VM** (if already created)
- **Automatic Docker & Docker Compose installation**
- **Complete OptimACS stack deployment** with one command

## Prerequisites

1. **Terraform** installed (>= 1.0)
2. **Proxmox** server with:
   - API access enabled
   - A cloud-init enabled VM template (e.g., Ubuntu 22.04 cloud image)
   - Sufficient resources (CPU, RAM, storage)
3. **SSH key pair** for VM access
4. **Configured `.env` file** with all required secrets

## Quick Start

### 1. Prepare Your Environment

```bash
cd terraform
cp terraform.tfvars.example terraform.tfvars
# Edit terraform.tfvars with your Proxmox and VM details
```

### 2. Ensure Your `.env` File is Ready

Your `.env` file should be in the parent directory with all required variables set:

```bash
cp ../.env.example ../.env
# Edit ../.env with your configuration
```

### 3. Initialize Terraform

```bash
terraform init
```

### 4. Review and Apply

```bash
terraform plan
terraform apply
```

### 5. Access Your Services

After deployment completes, you'll see URLs for:
- **Management UI**: http://vm-ip:8080
- **EMQX Dashboard**: http://vm-ip:18083
- **Grafana**: http://vm-ip:3000

## Configuration Reference

### Proxmox Settings

| Variable | Description | Default |
|----------|-------------|---------|
| `proxmox_api_url` | Proxmox API endpoint | (required) |
| `proxmox_user` | Proxmox user (root@pam) | (required) |
| `proxmox_password` | Proxmox password | (required) |
| `proxmox_node` | Target node name | `pve` |
| `proxmox_storage` | Storage pool | `local-lvm` |
| `proxmox_template` | VM template to clone | `debian-12-cloudinit` |

### VM Settings

| Variable | Description | Default |
|----------|-------------|---------|
| `provision_vm` | Create new VM or use existing | `true` |
| `vm_name` | Name for new VM | `optimacs-server` |
| `vm_cores` | CPU cores | `4` |
| `vm_memory` | RAM in MB | `8192` (8GB) |
| `vm_disk_size` | Disk size | `50G` |
| `vm_ipconfig` | Static IP (optional) | `` (DHCP) |

### SSH Settings

| Variable | Description | Default |
|----------|-------------|---------|
| `ssh_user` | SSH username | `debian` |
| `ssh_private_key_path` | Private key path | `~/.ssh/id_rsa` |
| `ssh_public_key_path` | Public key for cloud-init | `~/.ssh/id_rsa.pub` |

## Usage Examples

### Deploy to New Proxmox VM

```hcl
provision_vm       = true
proxmox_api_url    = "https://192.168.1.10:8006/api2/json"
proxmox_user       = "root@pam"
proxmox_password   = "secret"
proxmox_template   = "debian-12-cloudinit"
vm_name            = "optimacs-prod"
vm_cores           = 4
vm_memory          = 8192
vm_disk_size       = "100G"
```

### Deploy to Existing VM

```hcl
provision_vm       = false
vm_host            = "192.168.1.100"
ssh_user           = "ubuntu"
ssh_private_key_path = "~/.ssh/id_rsa"
```

### Static IP Configuration

```hcl
vm_ipconfig = "ip=192.168.1.50/24,gw=192.168.1.1"
```

## Post-Deployment Steps

### 1. Configure step-ca Provisioner

SSH into the VM and export the JWK provisioner key:

```bash
ssh -i ~/.ssh/id_rsa ubuntu@$(terraform output -raw vm_ip)
docker compose exec step-ca step ca provisioner list
# Export the key and update STEPCA_KID in .env
docker compose restart ac-server
```

### 2. Verify Services

```bash
docker compose ps
docker compose logs -f
```

### 3. Create Additional Users

```bash
docker compose exec optimacs-ui python create_admin.py \
  --username newadmin \
  --password secretpassword \
  --role super_admin
```

## Updating the Deployment

After making changes to `.env` or code:

```bash
terraform apply
```

To force a full redeploy:

```bash
terraform taint null_resource.optimacs_deploy
terraform apply
```

## Destroying Resources

To destroy the Proxmox VM and cleanup:

```bash
terraform destroy
```

**Warning**: This will delete the VM and all data. Back up important data first!

## Troubleshooting

### Proxmox Connection Issues

```bash
# Test Proxmox API access
curl -k -u "root@pam:password" \
  https://proxmox-host:8006/api2/json/cluster/resources
```

### VM Creation Fails

- Ensure the template exists: `qm list` on Proxmox
- Check template has cloud-init drive
- Verify storage pool has available space

### SSH Connection Fails

- Verify VM has cloud-init and SSH installed
- Check firewall rules on Proxmox host
- Ensure correct SSH key in `ssh_public_key_path`

### Docker/Docker Compose Not Found

The script attempts to install Docker automatically. If it fails:

```bash
ssh ubuntu@vm-ip
sudo apt-get update
sudo apt-get install -y docker.io docker-compose
sudo usermod -aG docker $USER
```

### Services Not Starting

Check logs on the VM:

```bash
ssh ubuntu@vm-ip
cd /opt/optimacs
docker compose logs
```

Common issues:
- Missing required environment variables in `.env`
- Port conflicts (8080, 18083, 3000, etc.)
- Insufficient disk space or memory

### Step-CA Certificate Issues

If ac-server fails to issue device certificates via step-ca:

```bash
# Check step-ca logs
ssh ubuntu@vm-ip
cd /opt/optimacs
docker compose logs step-ca

# Verify provisioner key exists
ls -la docker/stepca/provisioner.key

# Check ac-server can connect to step-ca
docker compose exec ac-server wget -qO- https://step-ca:9000/health
```

Ensure:
1. `STEPCA_PROVISIONER_NAME` is set correctly in `.env` (default: `acserver@optimacs.local`)
2. The provisioner key file `docker/stepca/provisioner.key` exists and is readable
3. The fingerprint in `.env` matches the step-ca root CA

## Security Notes

- Store `terraform.tfvars` securely (contains passwords)
- Add to `.gitignore`:
  ```
  *.tfvars
  *.tfstate
  *.tfstate.*
  .terraform/
  ```
- Use static IPs for production deployments
- Configure firewall rules on Proxmox host
- Change all default passwords before production use
- Consider using Terraform Cloud or Vault for secrets

## Cloud-Init Template Setup

If you don't have a cloud-init template, create one:

```bash
# On Proxmox node
wget https://cloud.debian.org/images/cloud/bookworm/latest/debian-12-generic-amd64.qcow2
qm create 9000 --name "debian-12-cloudinit" --memory 2048 --cores 2 --net0 virtio,bridge=vmbr0
qm importdisk 9000 debian-12-generic-amd64.qcow2 local-lvm
qm set 9000 --scsihw virtio-scsi-pci --scsi0 local-lvm:vm-9000-disk-0
qm set 9000 --ide2 local-lvm:cloudinit
qm set 9000 --boot order=scsi0
qm set 9000 --serial0 socket --vga serial0
qm template 9000
```

## Requirements

- Proxmox VE 7.0+
- Terraform >= 1.0
- VM Template with:
  - Debian 11+ or Ubuntu 20.04+
  - cloud-init
  - QEMU Guest Agent
  - SSH server
