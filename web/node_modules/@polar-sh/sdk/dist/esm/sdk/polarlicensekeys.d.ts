import { ClientSDK, RequestOptions } from "../lib/sdks.js";
import { LicenseKeyActivate } from "../models/components/licensekeyactivate.js";
import { LicenseKeyActivationRead } from "../models/components/licensekeyactivationread.js";
import { LicenseKeyDeactivate } from "../models/components/licensekeydeactivate.js";
import { LicenseKeyValidate } from "../models/components/licensekeyvalidate.js";
import { LicenseKeyWithActivations } from "../models/components/licensekeywithactivations.js";
import { ValidatedLicenseKey } from "../models/components/validatedlicensekey.js";
import { CustomerPortalLicenseKeysGetRequest, CustomerPortalLicenseKeysGetSecurity } from "../models/operations/customerportallicensekeysget.js";
import { CustomerPortalLicenseKeysListRequest, CustomerPortalLicenseKeysListResponse, CustomerPortalLicenseKeysListSecurity } from "../models/operations/customerportallicensekeyslist.js";
import { PageIterator } from "../types/operations.js";
export declare class PolarLicenseKeys extends ClientSDK {
    /**
     * List License Keys
     *
     * @remarks
     * **Scopes**: `customer_portal:read` `customer_portal:write`
     */
    list(security: CustomerPortalLicenseKeysListSecurity, request: CustomerPortalLicenseKeysListRequest, options?: RequestOptions): Promise<PageIterator<CustomerPortalLicenseKeysListResponse, {
        page: number;
    }>>;
    /**
     * Get License Key
     *
     * @remarks
     * Get a license key.
     *
     * **Scopes**: `customer_portal:read` `customer_portal:write`
     */
    get(security: CustomerPortalLicenseKeysGetSecurity, request: CustomerPortalLicenseKeysGetRequest, options?: RequestOptions): Promise<LicenseKeyWithActivations>;
    /**
     * Validate License Key
     *
     * @remarks
     * Validate a license key.
     */
    validate(request: LicenseKeyValidate, options?: RequestOptions): Promise<ValidatedLicenseKey>;
    /**
     * Activate License Key
     *
     * @remarks
     * Activate a license key instance.
     */
    activate(request: LicenseKeyActivate, options?: RequestOptions): Promise<LicenseKeyActivationRead>;
    /**
     * Deactivate License Key
     *
     * @remarks
     * Deactivate a license key instance.
     */
    deactivate(request: LicenseKeyDeactivate, options?: RequestOptions): Promise<void>;
}
//# sourceMappingURL=polarlicensekeys.d.ts.map