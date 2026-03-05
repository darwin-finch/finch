import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { LicenseKeyDeactivate } from "../models/components/licensekeydeactivate.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Deactivate License Key
 *
 * @remarks
 * Deactivate a license key instance.
 */
export declare function customerPortalLicenseKeysDeactivate(client: PolarCore, request: LicenseKeyDeactivate, options?: RequestOptions): APIPromise<Result<void, ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=customerPortalLicenseKeysDeactivate.d.ts.map