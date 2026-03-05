import { ClientSDK, RequestOptions } from "../lib/sdks.js";
import { LicenseKeyActivationRead } from "../models/components/licensekeyactivationread.js";
import { LicenseKeyRead } from "../models/components/licensekeyread.js";
import { LicenseKeyWithActivations } from "../models/components/licensekeywithactivations.js";
import { LicenseKeysGetRequest } from "../models/operations/licensekeysget.js";
import { LicenseKeysGetActivationRequest } from "../models/operations/licensekeysgetactivation.js";
import { LicenseKeysListRequest, LicenseKeysListResponse } from "../models/operations/licensekeyslist.js";
import { LicenseKeysUpdateRequest } from "../models/operations/licensekeysupdate.js";
import { PageIterator } from "../types/operations.js";
export declare class LicenseKeys extends ClientSDK {
    /**
     * List License Keys
     *
     * @remarks
     * Get license keys connected to the given organization & filters.
     *
     * **Scopes**: `license_keys:read` `license_keys:write`
     */
    list(request: LicenseKeysListRequest, options?: RequestOptions): Promise<PageIterator<LicenseKeysListResponse, {
        page: number;
    }>>;
    /**
     * Get License Key
     *
     * @remarks
     * Get a license key.
     *
     * **Scopes**: `license_keys:read` `license_keys:write`
     */
    get(request: LicenseKeysGetRequest, options?: RequestOptions): Promise<LicenseKeyWithActivations>;
    /**
     * Update License Key
     *
     * @remarks
     * Update a license key.
     *
     * **Scopes**: `license_keys:write`
     */
    update(request: LicenseKeysUpdateRequest, options?: RequestOptions): Promise<LicenseKeyRead>;
    /**
     * Get Activation
     *
     * @remarks
     * Get a license key activation.
     *
     * **Scopes**: `license_keys:read` `license_keys:write`
     */
    getActivation(request: LicenseKeysGetActivationRequest, options?: RequestOptions): Promise<LicenseKeyActivationRead>;
}
//# sourceMappingURL=licensekeys.d.ts.map