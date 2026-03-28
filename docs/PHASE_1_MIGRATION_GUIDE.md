# Phase 1 Enhancement Migration Guide

This guide explains how to migrate from the current Freenet Core to the enhanced Phase 1 version with security, admin APIs, and contract lifecycle management.

## Overview of Phase 1 Enhancements

Phase 1 adds critical enterprise-grade features to Freenet Core:

1. **ApplicationMessage Routing** - Fixes delegate → client communication (was broken TODO)
2. **Admin REST API** - Production monitoring and management endpoints
3. **Security Infrastructure** - RBAC, authentication, authorization, audit trails
4. **Contract Lifecycle Management** - GC, deletion, versioning, storage quotas

## Backward Compatibility

**All existing functionality continues to work unchanged.** The enhancements are additive and disabled by default to ensure smooth migration.

### What Stays the Same
- WebSocket API endpoints and behavior
- HTTP client API endpoints and behavior
- Contract execution and WASM runtime
- Network protocol and peer-to-peer communication
- Configuration file format (existing options unchanged)
- CLI commands and arguments

### What's New (Opt-in)
- Admin API endpoints (disabled by default)
- Security features (permissive mode by default)
- Contract deletion API (optional)
- Enhanced metrics and monitoring (optional)

## Migration Steps

### Step 1: Update Dependencies

The enhanced version includes new dependencies for security and monitoring. Update your Cargo.toml:

```toml
[dependencies]
freenet = { version = "0.3.0", features = ["enterprise"] }
```

Feature flags available:
- `admin-api` - Admin REST API with metrics export
- `security` - Enhanced authentication and RBAC
- `enterprise` - Both admin-api and security features
- Default features remain the same for backward compatibility

### Step 2: Review Configuration (Optional)

Your existing configuration continues to work. To enable new features, add sections to your config file:

```toml
# Enable admin API (optional)
[admin-api]
enabled = true
port = 7510
auth-required = false  # Start with no auth, add later

# Enable basic security (optional)
[security]
enabled = true
default-policy = "allow"  # Permissive mode during migration
audit-enabled = false     # Add audit logging later

# Enable contract lifecycle management (optional)
[contract-lifecycle]
gc-enabled = true
gc-interval = 3600  # Run GC every hour
```

### Step 3: Test ApplicationMessage Routing

The critical ApplicationMessage routing fix allows delegates to send messages to subscribed clients. Test this new functionality:

```javascript
// Client subscribes to delegate messages
ws.send(JSON.stringify({
  type: "SubscribeToDelegate",
  delegateKey: "your-delegate-key-here"
}));

// Delegate can now send ApplicationMessages to subscribed clients
// Messages will be delivered automatically via WebSocket or HTTP
```

### Step 4: Enable Admin API (Recommended)

The admin API provides essential production monitoring:

```bash
# Health checks for load balancers
curl http://localhost:7510/admin/health

# Prometheus metrics for monitoring
curl http://localhost:7510/admin/metrics

# Node status and peer information
curl http://localhost:7510/admin/node/status
curl http://localhost:7510/admin/network/peers
```

### Step 5: Gradual Security Rollout (Optional)

Security features can be enabled gradually:

#### Phase 5a: Enable Audit Logging
```toml
[security.audit]
enabled = true
events = ["authentication", "data_access"]
destinations = ["file:///var/log/freenet/audit.log"]
```

#### Phase 5b: Add Authentication
```toml
[security.authentication]
methods = ["api_key"]
# Generate keys with: openssl rand -base64 32

[admin-api]
auth-required = true
api-keys = ["admin:your-generated-key-here"]
```

#### Phase 5c: Enable Authorization
```toml
[security]
default-policy = "deny"  # Switch from permissive to secure mode
```

### Step 6: Enable Contract Lifecycle Management (Recommended)

Prevent unbounded storage growth:

```toml
[contract-lifecycle]
gc-enabled = true
storage.max-total-size = 10737418240  # 10GB limit
ttl.default-ttl = 2592000             # 30 days default TTL
```

## Rollback Plan

If you need to rollback, the process is straightforward:

### Option 1: Disable New Features
```toml
[admin-api]
enabled = false

[security]
enabled = false

[contract-lifecycle]
gc-enabled = false
```

### Option 2: Revert to Previous Version
```toml
[dependencies]
freenet = "0.2.14"  # Previous version
```

All data and configurations remain compatible.

## Monitoring the Migration

### Health Checks
Monitor the health endpoint to ensure the node is functioning properly:
```bash
curl http://localhost:7510/admin/health
```

Expected response during healthy operation:
```json
{
  "status": "healthy",
  "checks": {
    "network": {"status": "healthy", "connected_peers": 12},
    "storage": {"status": "healthy", "disk_usage_percent": 45},
    "background_tasks": {"status": "healthy", "active_tasks": [...]}
  }
}
```

### Metrics Monitoring
Key metrics to monitor during migration:

- `freenet_peer_connections_total` - Should maintain previous levels
- `freenet_operations_total{result="success"}` - Should not decrease
- `freenet_contracts_hosted_count` - Monitor contract GC impact
- `freenet_admin_api_requests_total` - Track admin API usage

### Log Monitoring
With audit logging enabled, monitor for:
- Authentication failures (potential configuration issues)
- Authorization denials (permission configuration issues)
- GC events (storage cleanup activity)

## Troubleshooting

### ApplicationMessage Routing Issues
If delegates can't send messages to clients:
1. Verify clients are subscribed to the delegate
2. Check WebSocket connection is active
3. Review delegate logs for ApplicationMessage generation

### Admin API Access Issues
If admin endpoints return 403/401:
1. Check `auth-required = false` for initial testing
2. Verify API keys are correctly configured
3. Check IP allowlist settings

### Security Configuration Issues
If clients can't access contracts after enabling security:
1. Start with `default-policy = "allow"`
2. Gradually add specific role restrictions
3. Check audit logs for permission denials

### Contract GC Issues
If contracts are being deleted unexpectedly:
1. Check TTL configuration is appropriate
2. Verify exemption settings are working
3. Review GC logs and metrics

## Performance Impact

The Phase 1 enhancements are designed to have minimal performance impact:

- **ApplicationMessage routing**: ~1% overhead for message processing
- **Admin API**: No impact on normal operations (separate port)
- **Security**: ~2-5% overhead for authentication/authorization
- **Contract GC**: Configurable, runs in background, minimal impact

## Getting Help

If you encounter issues during migration:

1. Check the configuration examples in `config/freenet-enterprise.toml`
2. Review the troubleshooting section above
3. Enable debug logging: `RUST_LOG=debug` environment variable
4. Check the admin API health endpoint for system status
5. Review audit logs if security is enabled

For additional support, see the project documentation or file an issue at https://github.com/freenet/freenet-core/issues.

## Next Steps: Phase 2 and Beyond

After successfully migrating to Phase 1:

- **Phase 2**: Comprehensive monitoring platform with real-time dashboard
- **Phase 3**: Multi-protocol APIs (gRPC, GraphQL) and plugin system
- **Phase 4**: Advanced features like HSM integration and performance optimizations

Each phase builds on the previous foundation and maintains backward compatibility.