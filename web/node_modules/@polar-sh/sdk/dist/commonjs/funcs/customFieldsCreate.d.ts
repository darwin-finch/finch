import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { CustomField } from "../models/components/customfield.js";
import { CustomFieldCreate } from "../models/components/customfieldcreate.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Create Custom Field
 *
 * @remarks
 * Create a custom field.
 *
 * **Scopes**: `custom_fields:write`
 */
export declare function customFieldsCreate(client: PolarCore, request: CustomFieldCreate, options?: RequestOptions): APIPromise<Result<CustomField, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=customFieldsCreate.d.ts.map