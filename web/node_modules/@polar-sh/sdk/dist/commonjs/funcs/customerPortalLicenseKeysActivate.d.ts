import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { LicenseKeyActivate } from "../models/components/licensekeyactivate.js";
import { LicenseKeyActivationRead } from "../models/components/licensekeyactivationread.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { NotPermitted } from "../models/errors/notpermitted.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Activate License Key
 *
 * @remarks
 * Activate a license key instance.
 */
export declare function customerPortalLicenseKeysActivate(client: PolarCore, request: LicenseKeyActivate, options?: RequestOptions): APIPromise<Result<LicenseKeyActivationRead, NotPermitted | ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=customerPortalLicenseKeysActivate.d.ts.map