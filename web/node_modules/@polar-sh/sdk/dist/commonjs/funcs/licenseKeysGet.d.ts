import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { LicenseKeyWithActivations } from "../models/components/licensekeywithactivations.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { Unauthorized } from "../models/errors/unauthorized.js";
import { LicenseKeysGetRequest } from "../models/operations/licensekeysget.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Get License Key
 *
 * @remarks
 * Get a license key.
 *
 * **Scopes**: `license_keys:read` `license_keys:write`
 */
export declare function licenseKeysGet(client: PolarCore, request: LicenseKeysGetRequest, options?: RequestOptions): APIPromise<Result<LicenseKeyWithActivations, Unauthorized | ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=licenseKeysGet.d.ts.map